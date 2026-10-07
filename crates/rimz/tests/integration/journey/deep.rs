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

use super::{rimz_bin, session_start_at};
use crate::common::{
    CommandTimeoutExt, Env, ScrubSessionEnvExt, ZellijNamespace, path_with_front,
    write_hook_firing_agent,
};

#[cfg(target_os = "linux")]
mod room_host;

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
        "RIMZ_HOME={rimz_home} XDG_STATE_HOME={state} XDG_CONFIG_HOME={config} XDG_RUNTIME_DIR={runtime} HOME={home} \
         {extra_env}RIMZ_BIN={rimz} exec {rimz} sidebar serve --mux {mux} --workspace-id {ws} \
         --session-name {session} --tick-seconds 1",
        rimz_home = env.rimz_home().display(),
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
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(rimz) = rimz_bin() else {
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
        crate::common::skip("git unavailable");
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

#[test]
#[expect(
    clippy::print_stderr,
    reason = "record the measured client-byte guard evidence"
)]
fn tmux_cell_only_sidebar_bounds_client_bytes() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(rimz) = rimz_bin() else {
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    const REFRESH_MS: u64 = 100;
    const BYTES_PER_FRAME: u64 = 1024;
    const STATUS_SLACK: u64 = 4096;
    std::fs::write(
        env.rimz_home().join("theme.toml"),
        "[theme.pets]\nenabled = false\n[theme.display]\npixel = 'off'\n",
    )
    .expect("cell-only sidebar theme");
    let runtime = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("short runtime dir");
    let socket = managed_socket(runtime.path());
    let _server = TmuxServerGuard::new(socket.clone());
    let fake_codex = fake_codex_bin(runtime.path());
    tmux(
        &socket,
        &[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "room",
            "-x",
            "200",
            "-y",
            "120",
            "-c",
            &env.project_root.display().to_string(),
            &format!("{} 300", fake_codex.display()),
        ],
    );
    tmux(
        &socket,
        &["set-option", "-as", "terminal-features", "*:sync"],
    );
    let codex_pane = tmux_capture(&socket, &["list-panes", "-t", "room", "-F", "#{pane_id}"]);
    let codex_pid = tmux_capture(
        &socket,
        &["display-message", "-p", "-t", &codex_pane, "#{pane_pid}"],
    );
    let serve = format!(
        "{} --refresh-ms {REFRESH_MS}",
        sidebar_serve_line(&env, &rimz, runtime.path(), "tmux", "room", &[])
    );
    let sidebar = tmux_capture(
        &socket,
        &[
            "split-window",
            "-h",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "room",
            &serve,
        ],
    );
    let parser = Arc::new(Mutex::new(vt100::Parser::new(120, 200, 0)));
    let mut attach = CommandBuilder::new("tmux");
    env.pin_pty_command(&mut attach);
    attach.env("TERM", "xterm-256color");
    attach.args([
        "-u",
        "-S",
        socket.to_str().expect("utf8 socket"),
        "attach",
        "-t",
        "room",
    ]);
    let _client = AttachProcess::on_pty(attach, &parser);

    env.install_agent_hooks("codex");
    let hook_env = [
        ("TMUX_PANE", codex_pane.as_str()),
        ("RIMZ_AGENT_PID", codex_pid.as_str()),
        (rimz::harness::launch::ENV_AGENT_ROLE, "coder"),
    ];
    for payload in [
        session_start_at(
            "wire-agent",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            None,
        ),
        serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "wire-agent",
            "prompt": "keep working while the sidebar animates",
        }),
    ] {
        let out = env.run_installed_hook_in_pane("codex", &payload.to_string(), &hook_env);
        assert!(out.status.success(), "codex hook failed: {out:?}");
    }
    let screen = capture_until(&socket, &sidebar, |s| s.contains("coder"), CAPTURE_BUDGET);
    assert!(
        screen.contains("coder"),
        "sidebar must paint the running agent: {screen}"
    );
    assert_eq!(
        env.store()
            .snapshot()
            .expect("agent snapshot")
            .agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "wire-agent")
            .expect("installed hook's agent")
            .status,
        rimz::agents::AgentStatus::Running,
    );
    let dimensions = tmux_capture(
        &socket,
        &[
            "display-message",
            "-p",
            "-t",
            &sidebar,
            "#{pane_width} #{pane_height}",
        ],
    );
    let cells: u64 = dimensions
        .split_whitespace()
        .map(|dimension| dimension.parse::<u64>().expect("pane dimension"))
        .product();
    assert!(
        cells >= 4000,
        "full repaint must exceed the diff budget: {dimensions}"
    );
    let client_written = || {
        tmux_capture(
            &socket,
            &[
                "list-clients",
                "-t",
                "room",
                "-f",
                "#{==:#{client_control_mode},0}",
                "-F",
                "#{client_written}",
            ],
        )
        .parse::<u64>()
        .expect("one attached client's byte counter")
    };
    let agent_row = || {
        parser
            .lock()
            .expect("attached client parser")
            .screen()
            .contents()
            .lines()
            .find(|line| line.contains("coder"))
            .unwrap_or_default()
            .to_owned()
    };
    let deadline = Instant::now() + CAPTURE_BUDGET;
    while agent_row().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let initial_row = agent_row();
    assert!(
        !initial_row.is_empty(),
        "the attached client must see the agent"
    );
    let screen = || {
        parser
            .lock()
            .expect("attached client parser")
            .screen()
            .contents_formatted()
    };
    let initial_screen = screen();
    let before = client_written();
    let started = Instant::now();
    let mut screen_changed = false;
    while started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(125));
        screen_changed |= screen() != initial_screen;
    }
    let after = client_written();
    let elapsed = started.elapsed();
    let bytes = after - before;
    let frames = elapsed.as_millis().div_ceil(u128::from(REFRESH_MS)) as u64;
    let budget = frames * BYTES_PER_FRAME + STATUS_SLACK;
    eprintln!(
        "cell-only sidebar: {bytes} client bytes in {elapsed:?}, {dimensions} cells, budget {budget}"
    );
    assert!(
        bytes > 0,
        "the attached client must receive paints during the window"
    );
    assert!(
        screen_changed,
        "the attached client must see the sidebar repaint during the window"
    );
    assert!(
        bytes <= budget,
        "cell-only frames must cost at most {BYTES_PER_FRAME} bytes each plus {STATUS_SLACK} status bytes: received {bytes}, budget {budget}",
    );
}

/// tmux: closing a work column returns its width to the remaining work panes,
/// not the fixed-width sidebar.
#[test]
fn tmux_sidebar_keeps_width_when_work_pane_closes() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(rimz) = rimz_bin() else {
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
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(rimz) = rimz_bin() else {
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
        crate::common::skip("zellij not on PATH");
        return;
    }
    let Some(rimz) = rimz_bin() else {
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
    // Record the room before the id-only renderer resolves its runtime directory.
    env.record(&env.project_root);
    let state = env.state_path_for(&env.project_root);
    let runtime_paths = rimz::RuntimePaths::for_state_under(&state, runtime);

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
    let view_cols = 320;
    let mut client = AttachedZellijScreen::new(&cleanup.namespace, name, view_cols, 80);
    let screen = client.wait_until(|screen| screen.contains("codex"), CAPTURE_BUDGET);
    assert!(
        screen.contains("codex"),
        "the live zellij sidebar pane should show the agent row:\n{screen}"
    );

    // Keep the raced feed in place long enough for the old controller to
    // converge against it, then publish the completed tab without a target
    // broadcast. The next keypress must resolve against this proven viewport.
    std::thread::sleep(Duration::from_secs(3));
    let layout_width =
        wait_for_rendered_sidebar_width(&mut client, |_| true, "layout sidebar width");
    write_zellij_topology(&cleanup.namespace, &runtime_paths, name, false);

    let backend = ZellijBackend::with_runtime_dir(runtime);
    let pane = wait_for_zellij_sidebar_pane(&backend, &runtime_paths, name);
    // The sidebar-only extent never nudges, so the first nudge measured
    // against the whole tab is the controller acting on the proven viewport.
    // The key must land after the frame repainted at that nudge's width.
    env.wait_for_diag(
        name,
        |record| {
            matches!(
                record.event,
                DiagEvent::SidebarWidthNudge { view_cols: nudged, .. } if nudged == view_cols
            )
        },
        CAPTURE_BUDGET,
    );
    let initial = wait_for_rendered_sidebar_width(
        &mut client,
        |width| width != layout_width,
        "sidebar frame at the automatic width",
    );

    backend.send_keys(&pane, name, "d").expect("send wider key");
    let wider = wait_for_rendered_sidebar_width(
        &mut client,
        |width| width > initial,
        "wider sidebar frame",
    );
    std::thread::sleep(Duration::from_secs(3));
    let held_wider = wait_for_rendered_sidebar_width(&mut client, |_| true, "settled wider frame");
    let diag = env.diag_tail(name, DIAG_EVIDENCE_RECORDS);
    assert_eq!(
        held_wider, wider,
        "the wider sidebar frame reverted after the convergence settle window\n{diag}",
    );

    backend
        .send_keys(&pane, name, "a")
        .expect("send narrower key");
    let narrower = wait_for_rendered_sidebar_width(
        &mut client,
        |width| width < held_wider,
        "narrower sidebar frame",
    );
    std::thread::sleep(Duration::from_secs(3));
    let held_narrower =
        wait_for_rendered_sidebar_width(&mut client, |_| true, "settled narrower frame");
    let diag = env.diag_tail(name, DIAG_EVIDENCE_RECORDS);
    assert_eq!(
        held_narrower, narrower,
        "the narrower sidebar frame reverted after the convergence settle window\n{diag}",
    );
}

#[test]
fn tmux_steer_delivers_text_and_enter_to_real_agent_pane() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
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
        crate::common::skip("tmux not on PATH");
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
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
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
    trust_agent_path(&env, "codex", &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let prompt = "summarize the diff; preserve \"quoted\" details\n".repeat(1024);
    let prompt = &prompt[..40 * 1024];

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
            prompt,
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
fn tmux_resumed_parent_session_switch_keeps_subagent_nested() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
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
    trust_agent_path(&env, "codex", &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let session = workspace_session(&env);

    let parent = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("RIMZ_TEST_AGENT_SESSION", "sess-parent-old")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "120000")
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "coordinate the review",
            "--name",
            "switch-parent",
            "-p",
            "--bg",
            "--keep",
            "--timeout",
            "3m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch parent");
    assert!(parent.status.success(), "parent launch failed: {parent:?}");
    let old = wait_for_named_agent(&env, "switch-parent", true, CAPTURE_BUDGET);
    assert_eq!(old.agent_id.as_str(), "sess-parent-old");
    let launch_id = old.launch_id.clone().expect("parent launch identity");
    let old_pid = old.runtime_owner.as_ref().expect("OLD provider owner").pid;
    let old_wrapper_pid = rimz::proc::comm_and_ppid(old_pid)
        .map(|(_, ppid)| ppid)
        .expect("OLD provider parent process");
    let old_pane = tmux_pane_for_pid(&socket, &session, old_wrapper_pid)
        .expect("OLD wrapper's actual pane, not its possibly stale hook stamp");
    // Retain the pane after the wrapper settles so resume reuses its address.
    tmux(
        &socket,
        &["set-option", "-p", "-t", &old_pane, "remain-on-exit", "on"],
    );
    let stopped = Command::new("kill")
        .args(["-TERM", &old_pid.to_string()])
        .bounded_output()
        .expect("stop OLD provider, leaving its wrapper to settle the run");
    assert!(stopped.status.success(), "stop OLD provider: {stopped:?}");
    let deadline = Instant::now() + CAPTURE_BUDGET;
    while rimz::proc::comm_and_ppid(old_pid).is_some() {
        assert!(Instant::now() < deadline, "OLD provider did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    wait_for_named_terminal_run(&env, "switch-parent", CAPTURE_BUDGET);
    // The shim has no SessionEnd trap. Supply the provider's real end hook
    // after its process exits, rather than manufacturing an ended store row.
    if env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("read stopped parent")
        .agents
        .iter()
        .any(|agent| agent.agent_id == old.agent_id && agent.ended_at.is_none())
    {
        let ended = env.run_installed_hook_in_pane(
            "codex",
            &serde_json::json!({
                "hook_event_name": "SessionEnd",
                "session_id": old.agent_id.as_str(),
            })
            .to_string(),
            &[
                ("TMUX", &tmux_env(&socket)),
                ("TMUX_PANE", &old_pane),
                ("RIMZ_AGENT_PID", &old_pid.to_string()),
            ],
        );
        assert!(ended.status.success(), "end OLD session: {ended:?}");
    }
    let deadline = Instant::now() + CAPTURE_BUDGET;
    loop {
        let snapshot = env
            .store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("read ended OLD");
        if snapshot
            .agents
            .iter()
            .any(|agent| agent.agent_id == old.agent_id && agent.ended_at.is_some())
        {
            break;
        }
        assert!(Instant::now() < deadline, "OLD did not end: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The shim ignores resume argv and registers the session selected by the
    // tmux server, just as a provider returning a forked rollout does.
    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-parent-new"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "120000"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    // Exercise the real resume exec/attach path, not cohort selection. Bare
    // `agents codex --resume` execs directly; close-on-exit forces spawn mode.
    let mut request = rimz::harness::launch::ExecRequest::bare_launch(old.kind.clone(), vec![]);
    request.action = rimz::harness::launch::ExecAction::Resume {
        session_id: old.agent_id.to_string(),
        extra_args: vec![],
    };
    request.identity.launch_id = Some(launch_id.to_string());
    request.close_pane_on_exit = true;
    let argv = rimz::harness::launch::exec_argv(&env.rimz_bin(), &env.runtime_paths(), &request)
        .expect("compile spawn-mode resume");
    let command = argv
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!("exec {command}");
    tmux(
        &socket,
        &[
            "respawn-pane",
            "-k",
            "-t",
            &old_pane,
            "-c",
            old.worktree_path.as_deref().expect("OLD cwd"),
            &command,
        ],
    );
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let new =
        loop {
            let snapshot = env.store().snapshot().expect("read resumed parent");
            if let Some(new) = snapshot.agents.iter().find(|agent| {
                agent.agent_id.as_str() == "sess-parent-new" && agent.holds_open_turn()
            }) {
                break new.clone();
            }
            assert!(
                Instant::now() < deadline,
                "resumed provider did not register NEW: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
    let provider_pid = new.runtime_owner.as_ref().expect("NEW provider owner").pid;
    let wrapper_pid = rimz::proc::comm_and_ppid(provider_pid)
        .map(|(_, ppid)| ppid)
        .expect("resumed provider parent process");
    let parent_pane = tmux_pane_for_pid(&socket, &session, wrapper_pid)
        .unwrap_or_else(|| panic!("resume wrapper must own a pane: provider={provider_pid}, wrapper={wrapper_pid}, wrapper_parent={:?}, old_pane={old_pane}", rimz::proc::comm_and_ppid(wrapper_pid)));
    assert_eq!(parent_pane, old_pane, "resume must reuse the actual pane");
    assert_ne!(provider_pid, wrapper_pid);
    let wrapper_argv = rimz::proc::argv(wrapper_pid).expect("resumed wrapper argv");
    assert!(
        wrapper_argv.get(1).and_then(|arg| arg.to_str()) == Some("agents")
            && wrapper_argv.get(2).and_then(|arg| arg.to_str()) == Some("exec"),
        "resumed provider must be spawned by rimz, not execed by a shell"
    );
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("read resumed identity");
    let attached_old = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id == old.agent_id)
        .expect("old parent history");
    assert_eq!(attached_old.launch_id.as_ref(), Some(&launch_id));
    assert_eq!(
        attached_old.pane.as_ref().map(|pane| &pane.pane_id),
        new.pane.as_ref().map(|pane| &pane.pane_id)
    );
    assert!(attached_old.ended_at.is_some());
    assert!(new.ended_at.is_none());

    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-switch-child"),
        ("RIMZ_TEST_SUBAGENT_PARENT_PROBE_INTERVAL_MS", "500"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let child = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::launch::ENV_AGENT_ID, launch_id.as_str())
        .env("RIMZ_TEST_SUBAGENT_PARENT_PROBE_INTERVAL_MS", "500")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "inspect after resume",
            "--description",
            "inspect after resume",
            "--timeout",
            "3m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch resumed parent's child");
    assert!(child.status.success(), "subagent launch failed: {child:?}");
    let child_name = launched_subagent_name(&child);
    let child_agent = wait_for_named_agent(&env, &child_name, true, CAPTURE_BUDGET);
    let child_provider_pid = child_agent.runtime_owner.as_ref().unwrap().pid;
    let child_wrapper_pid = rimz::proc::comm_and_ppid(child_provider_pid).unwrap().1;
    assert_eq!(
        rimz::proc::env_var(
            child_wrapper_pid,
            "RIMZ_TEST_SUBAGENT_PARENT_PROBE_INTERVAL_MS"
        )
        .as_deref(),
        Some("500"),
        "probe cadence must reach the child watchdog process"
    );
    let parent_stamp = new.pane.as_ref().expect("NEW pane");
    let child_pane = wait_for_named_run(&env, &child_name, CAPTURE_BUDGET)
        .pane_id
        .expect("child run pane");
    let mut parent_fixture = live_agent_pane_fixture(&new, parent_stamp);
    parent_fixture.pane_id = rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Tmux, &parent_pane);
    let mut child_fixture = live_agent_pane_fixture(&child_agent, parent_stamp);
    child_fixture.pane_id = child_pane;
    let snapshot = env.snapshot_json_with_panes(&[parent_fixture, child_fixture]);
    let rows = snapshot["worktree_groups"]
        .as_array()
        .expect("worktree groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .collect::<Vec<_>>();
    let parent_row = rows
        .iter()
        .find(|row| row["id"] == new.agent_id.as_str())
        .expect("live NEW must render a parent card");
    let nested = parent_row["sub_agents"].as_array().is_some_and(|children| {
        children
            .iter()
            .any(|child| child["description"] == "inspect after resume")
    });
    let root_child = rows
        .iter()
        .any(|row| row["id"] == child_agent.agent_id.as_str());

    // Collect the consumer outcome before asserting inheritance, so a red
    // run also distinguishes the watchdog defect from mere producer state.
    // Two seconds covers four configured 500 ms parent-probe intervals.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut stayed_live = true;
    loop {
        let run = wait_for_named_run(&env, &child_name, CAPTURE_BUDGET);
        stayed_live &= !run.status.is_terminal();
        if Instant::now() >= deadline {
            assert!(
                tmux_pane_alive(&socket, &session, &parent_pane),
                "NEW parent pane disappeared"
            );
            assert!(
                new.launch_id.as_ref() == Some(&launch_id)
                    && child_agent.parent_agent_id.as_ref() == Some(&launch_id)
                    && nested
                    && !root_child
                    && stayed_live,
                "resumed launch lost its child: expected_launch={launch_id}, NEW_launch={:?}, OLD_owner={:?}, NEW_owner={:?}, child_parent={:?}, nested={nested}, root_child={root_child}, stayed_live={stayed_live}, child_run={run:?}",
                new.launch_id,
                attached_old.runtime_owner,
                new.runtime_owner,
                child_agent.parent_agent_id,
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let before_exit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let current_parent =
        rimz::address::launch_row(&before_exit.agents, &new.kind, &launch_id).unwrap();
    assert_eq!(
        current_parent.pane.as_ref().unwrap().pane_id.raw(),
        parent_pane,
        "watchdog must see the actual parent pane before its death: launch_group={:?}, wrapper_tmux_pane={:?}, provider_tmux_pane={:?}, panes={}",
        before_exit
            .agents
            .iter()
            .filter(|agent| {
                agent.agent_id == launch_id || agent.launch_id.as_ref() == Some(&launch_id)
            })
            .map(|agent| (
                &agent.kind,
                &agent.agent_id,
                &agent.launch_id,
                agent.ended_at,
                agent.holds_open_turn(),
                &agent.pane,
            ))
            .collect::<Vec<_>>(),
        rimz::proc::env_var(wrapper_pid, "TMUX_PANE"),
        rimz::proc::env_var(provider_pid, "TMUX_PANE"),
        tmux_capture(
            &socket,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id} #{pane_pid} #{pane_current_command}"
            ],
        ),
    );
    tmux(&socket, &["kill-pane", "-t", &parent_pane]);
    assert!(!tmux_pane_alive(&socket, &session, &parent_pane));
    let ended = wait_for_named_terminal_run(&env, &child_name, CAPTURE_BUDGET);
    assert_eq!(ended.status, rimz::store::run::RunStatus::Canceled);
}

#[test]
fn tmux_subagent_nests_under_parent_and_parent_stop_cascades() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
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
    trust_agent_path(&env, "codex", &agent_path);
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
            "--description",
            "inspect the implementation",
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
    let child_name = launched_subagent_name(&child);

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
            "--description",
            "review the implementation",
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
    let sibling_name = launched_subagent_name(&sibling);

    let child_agent = wait_for_named_agent(&env, &child_name, true, CAPTURE_BUDGET);
    assert_eq!(
        child_agent.parent_agent_id.as_ref(),
        Some(&parent_launch_id)
    );
    assert_eq!(child_agent.launch_depth, Some(1));
    let sibling_agent = wait_for_named_agent(&env, &sibling_name, true, CAPTURE_BUDGET);
    assert_eq!(
        sibling_agent.parent_agent_id.as_ref(),
        Some(&parent_launch_id)
    );
    let child_pane = wait_for_named_run(&env, &child_name, CAPTURE_BUDGET)
        .pane_id
        .expect("child run pane");
    let sibling_pane = wait_for_named_run(&env, &sibling_name, CAPTURE_BUDGET)
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

    let parent_stamp = parent_agent.pane.clone().expect("parent provider pane");
    let parent_fixture = live_agent_pane_fixture(&parent_agent, &parent_stamp);
    let snapshot = env.snapshot_json_with_panes(&[parent_fixture]);
    let rows = snapshot["worktree_groups"]
        .as_array()
        .expect("snapshot worktree groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("snapshot worktree rows"))
        .collect::<Vec<_>>();
    let parent_row = rows
        .iter()
        .find(|row| row["id"] == parent_agent.agent_id.as_str())
        .expect("parent row");
    let sub_agents = parent_row["sub_agents"]
        .as_array()
        .expect("nested children");
    assert!(
        sub_agents.len() == 2
            && sub_agents
                .iter()
                .any(|child| child["description"] == "inspect the implementation")
            && sub_agents
                .iter()
                .any(|child| child["description"] == "review the implementation"),
        "children should project into the parent's subagent section: {snapshot:#}"
    );
    assert!(
        rows.iter().all(|row| {
            row["id"] != child_agent.agent_id.as_str()
                && row["id"] != sibling_agent.agent_id.as_str()
        }),
        "children should not also project as top-level cards: {snapshot:#}"
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
fn tmux_settled_subagent_reports_to_parent() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
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
    trust_agent_path(&env, "codex", &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let session = workspace_session(&env);
    let started = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "tmux", "start", "--no-attach"])
        .bounded_output_within(Duration::from_secs(45))
        .expect("start report room");
    assert!(
        started.status.success(),
        "room start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch_pane = tmux_capture(
        &socket,
        &[
            "list-panes",
            "-t",
            &session,
            "-F",
            "#{pane_id}:#{pane_title}",
        ],
    )
    .lines()
    .find_map(|line| {
        let (pane, title) = line.split_once(':')?;
        (title != rimz::pane::SIDEBAR_CHROME_TITLE).then(|| pane.to_owned())
    })
    .expect("room shell pane");
    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-report-parent"),
        ("RIMZ_TEST_AGENT_WAIT_STDIN", "1"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }

    let parent = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &launch_pane)
        .env("RIMZ_TEST_AGENT_SESSION", "sess-report-parent")
        .env("RIMZ_TEST_AGENT_WAIT_STDIN", "1")
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "coordinate the reports",
            "--name",
            "report-parent",
            "--bg",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch report parent");
    assert!(
        parent.status.success(),
        "parent launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&parent.stdout),
        String::from_utf8_lossy(&parent.stderr)
    );

    let parent_agent = wait_for_named_agent(&env, "report-parent", true, CAPTURE_BUDGET);
    let parent_launch_id = parent_agent
        .launch_id
        .clone()
        .expect("RimZ-launched parent has launch id");
    let parent_pane_raw = parent_agent
        .pane
        .as_ref()
        .expect("parent provider pane")
        .pane_id
        .raw()
        .to_owned();

    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-report-second"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "10000"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let second = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-report-second")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "10000")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "stay running",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch second subagent");
    assert!(second.status.success(), "second launch failed: {second:?}");
    let second_name = launched_subagent_name(&second);
    wait_for_named_run(&env, &second_name, CAPTURE_BUDGET);

    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-report-first"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "0"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let first = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-report-first")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "0")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "finish now",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch first subagent");
    assert!(
        first.status.success(),
        "first launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stderr).contains(
            "one SUBAGENT_REPORT reaches you once every subagent you launched has settled"
        ),
        "background launch should explain callback delivery: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_name = launched_subagent_name(&first);
    let first_run = wait_for_named_terminal_run(&env, &first_name, CAPTURE_BUDGET);
    assert_eq!(
        first_run.report_message_id, None,
        "the first settler must wait for the rest of the fleet"
    );
    assert!(
        env.store()
            .list_messages()
            .expect("list queued reports")
            .iter()
            .all(|message| !matches!(
                &message.sender,
                rimz::store::message::MessageSender::Harness { .. }
            )),
        "no digest should queue while a sibling is running"
    );

    let second_handle = format!("@{second_name}");
    let stopped = env
        .rimz()
        .env("TMUX", tmux_env(&socket))
        .args(["--mux", "tmux", "agents", "stop", &second_handle])
        .bounded_output_within(Duration::from_secs(20))
        .expect("stop second subagent");
    assert!(stopped.status.success(), "stop second failed: {stopped:?}");
    let second_run = wait_for_named_terminal_run(&env, &second_name, CAPTURE_BUDGET);
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let fleet_digest = loop {
        if let Some(report) = env
            .store()
            .list_messages()
            .expect("list fleet digest")
            .into_iter()
            .find(|message| {
                message.status == rimz::store::message::MessageStatus::Sent
                    && matches!(
                        message.sender,
                        rimz::store::message::MessageSender::Harness {
                            notice: rimz::store::message::HarnessNotice::SubagentReport
                        }
                    )
            })
        {
            break report;
        }
        assert!(Instant::now() < deadline, "fleet digest was not queued");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(fleet_digest.text.contains("All 2 subagents settled"));
    assert!(
        fleet_digest
            .text
            .contains(&format!("@{first_name}: completed"))
    );
    assert!(
        fleet_digest
            .text
            .contains(&format!("@{second_name}: canceled"))
    );
    assert!(!fleet_digest.text.contains("rimz subagents wait"));
    let response_path = rimz::harness::run::response_path(env.store().paths(), &first_run).unwrap();
    let response = std::fs::read_to_string(&response_path).expect("published child response");
    assert_eq!(
        response.trim_end_matches('\n'),
        first_run
            .last_message
            .as_deref()
            .unwrap()
            .trim_end_matches('\n')
    );
    let summary =
        rimz::disk::summary::FileSummary::measure(&response_path).expect("response summary");
    assert!(fleet_digest.text.contains(&format!(
        "response: {} ({})",
        response_path.display(),
        summary.label()
    )));
    assert!(fleet_digest.text.contains("task: \"finish now\""));
    for run in [&first_run, &second_run] {
        assert_eq!(
            rimz::harness::run::load(env.store().paths(), &run.run_id)
                .expect("reload reported run")
                .report_message_id,
            Some(fleet_digest.message_id.clone())
        );
    }

    let first_digest_line = format!("@{first_name}: completed");
    let second_digest_line = format!("@{second_name}: canceled");
    let parent_frame = capture_joined_until(
        &socket,
        &parent_pane_raw,
        |frame| {
            frame.contains("Type: SUBAGENT_REPORT")
                && frame.contains("From: @rimz")
                && frame.contains(&first_digest_line)
                && frame.contains(&second_digest_line)
        },
        CAPTURE_BUDGET,
    );
    assert!(
        parent_frame.contains("Type: SUBAGENT_REPORT")
            && parent_frame.contains("From: @rimz")
            && parent_frame.contains(&first_digest_line)
            && parent_frame.contains(&second_digest_line)
            && !parent_frame.contains("stub done"),
        "parent pane did not receive status-only digests:\n{parent_frame}"
    );

    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-report-waited"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "0"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    // A real inline join runs during the parent's turn. The stdin-only stub
    // otherwise stays stopped, allowing delivery to beat join cancellation;
    // an already-sent digest cannot be recalled.
    let parent_pid = tmux_capture(
        &socket,
        &[
            "display-message",
            "-p",
            "-t",
            &parent_pane_raw,
            "#{pane_pid}",
        ],
    );
    let parent_hook_env = [
        ("TMUX_PANE", parent_pane_raw.as_str()),
        ("RIMZ_AGENT_PID", parent_pid.as_str()),
    ];
    let resumed = env.run_installed_hook_in_pane(
        "codex",
        &serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": parent_agent.agent_id.as_str(),
            "prompt": format!("Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\n{}", fleet_digest.text),
        })
        .to_string(),
        &parent_hook_env,
    );
    assert!(resumed.status.success(), "resume parent: {resumed:?}");
    assert_eq!(
        wait_for_named_agent(&env, "report-parent", true, CAPTURE_BUDGET).status,
        rimz::agents::AgentStatus::Running,
        "the report delivery gate must see an active parent turn"
    );
    assert_eq!(
        env.store()
            .list_message_history()
            .expect("read digest receipt")
            .iter()
            .find(|message| message.message_id == fleet_digest.message_id)
            .expect("received fleet digest")
            .status,
        rimz::store::message::MessageStatus::Delivered
    );
    let waited = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-report-waited")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "0")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "join this result",
            "--timeout",
            "2m",
            "--wait",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch waited subagent");
    assert!(
        waited.status.success(),
        "waited launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&waited.stdout),
        String::from_utf8_lossy(&waited.stderr)
    );
    assert!(String::from_utf8_lossy(&waited.stdout).contains("stub done"));
    let waited_name = launched_subagent_name(&waited);
    let waited_run = wait_for_named_terminal_run(&env, &waited_name, CAPTURE_BUDGET);
    assert!(
        waited_run.joined_at.is_some(),
        "--wait must mark the printed result joined: {waited_run:?}"
    );
    assert_eq!(
        env.store()
            .list_messages()
            .expect("list reports after waited launch")
            .iter()
            .filter(|message| {
                message.status != rimz::store::message::MessageStatus::Canceled
                    && matches!(
                        &message.sender,
                        rimz::store::message::MessageSender::Harness { .. }
                    )
            })
            .count(),
        0,
        "--wait must leave no new digest queued during the parent's turn"
    );

    for run in [&first_run, &waited_run] {
        assert!(
            tmux_pane_alive(&socket, &session, run.pane_id.as_ref().unwrap().raw()),
            "received child must stay alive during the parent's receiving turn"
        );
    }

    let stopped = env.run_installed_hook_in_pane(
        "codex",
        &serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": parent_agent.agent_id.as_str(),
            "last_assistant_message": "joined the child",
        })
        .to_string(),
        &parent_hook_env,
    );
    assert!(stopped.status.success(), "stop parent turn: {stopped:?}");
    assert!(
        !wait_for_named_agent(&env, "report-parent", true, CAPTURE_BUDGET).holds_open_turn(),
        "the unattended wait must run after the parent turn ended"
    );
    for run in [&first_run, &waited_run] {
        let pane = run.pane_id.as_ref().unwrap().raw();
        let deadline = Instant::now() + CAPTURE_BUDGET;
        while Instant::now() < deadline && tmux_pane_alive(&socket, &session, pane) {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !tmux_pane_alive(&socket, &session, pane),
            "child pane must close after the parent's receiving turn ends"
        );
    }

    // The stdin-only stub echoes pasted digests but fires no receiving hooks.
    let receive_and_finish = |run: &rimz::store::run::RunRecord| {
        let reported_run = rimz::harness::run::load(env.store().paths(), &run.run_id)
            .expect("reload reported child");
        let report = env
            .store()
            .list_messages()
            .expect("read sent digest")
            .into_iter()
            .find(|message| Some(&message.message_id) == reported_run.report_message_id.as_ref())
            .expect("child digest awaiting receipt");
        for event in ["UserPromptSubmit", "Stop"] {
            let output = env.run_installed_hook_in_pane(
                "codex",
                &serde_json::json!({
                    "hook_event_name": event,
                    "session_id": parent_agent.agent_id.as_str(),
                    "prompt": format!("Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\n{}", report.text),
                    "last_assistant_message": "received the child result",
                })
                .to_string(),
                &parent_hook_env,
            );
            assert!(output.status.success(), "parent {event}: {output:?}");
        }
        let pane = run.pane_id.as_ref().unwrap().raw();
        let deadline = Instant::now() + CAPTURE_BUDGET;
        while Instant::now() < deadline && tmux_pane_alive(&socket, &session, pane) {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !tmux_pane_alive(&socket, &session, pane),
            "child pane must close after the parent's receiving turn ends"
        );
    };

    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-report-unattended"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "0"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let unattended = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-report-unattended")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "0")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "print after the parent turn ended",
            "--timeout",
            "2m",
            "--wait",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch unattended waited subagent");
    assert!(
        unattended.status.success(),
        "unattended wait failed: {unattended:?}"
    );
    assert!(String::from_utf8_lossy(&unattended.stdout).contains("stub done"));
    let unattended_name = launched_subagent_name(&unattended);
    let unattended_run = wait_for_named_terminal_run(&env, &unattended_name, CAPTURE_BUDGET);
    assert_eq!(
        unattended_run.joined_at, None,
        "a printed result after the parent's turn must remain unjoined: {unattended_run:?}"
    );
    let unattended_digest_line = format!("@{unattended_name}: completed");
    let unattended_frame = capture_joined_until(
        &socket,
        &parent_pane_raw,
        |frame| {
            frame.contains("Type: SUBAGENT_REPORT")
                && frame.contains("From: @rimz")
                && frame.contains(&unattended_digest_line)
        },
        CAPTURE_BUDGET,
    );
    assert!(
        unattended_frame.contains("Type: SUBAGENT_REPORT")
            && unattended_frame.contains("From: @rimz")
            && unattended_frame.contains(&unattended_digest_line)
            && !unattended_frame.contains("stub done"),
        "unattended join suppressed the parent's native digest: {unattended_run:?}\n{unattended_frame}"
    );
    receive_and_finish(&unattended_run);

    for (key, value) in [
        ("RIMZ_TEST_AGENT_SESSION", "sess-report-timed"),
        ("RIMZ_TEST_AGENT_SLEEP_MS", "2500"),
    ] {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let timed = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane_raw)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .env("RIMZ_TEST_AGENT_SESSION", "sess-report-timed")
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "2500")
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "finish after the join deadline",
            "--timeout",
            "2m",
            "--wait=1s",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch deadline-limited subagent");
    assert_eq!(
        timed.status.code(),
        Some(rimz::store::run::RunStatus::TimedOut.exit_code()),
        "deadline-limited join should time out\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&timed.stdout),
        String::from_utf8_lossy(&timed.stderr)
    );
    let timed_name = launched_subagent_name(&timed);
    let timed_run = wait_for_named_terminal_run(&env, &timed_name, CAPTURE_BUDGET);
    assert_eq!(
        timed_run.joined_at, None,
        "a join deadline must not claim an unprinted result"
    );
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let timed_report = loop {
        if let Some(report) = env
            .store()
            .list_messages()
            .expect("list reports after join deadline")
            .into_iter()
            .find(|message| {
                message.status == rimz::store::message::MessageStatus::Sent
                    && matches!(
                        &message.sender,
                        rimz::store::message::MessageSender::Harness {
                            notice: rimz::store::message::HarnessNotice::SubagentReport
                        }
                    )
                    && message.text.contains(&format!("@{timed_name}: completed"))
            })
        {
            break report;
        }
        assert!(
            Instant::now() < deadline,
            "deadline-limited child did not queue a later report; run: {timed_run:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(timed_report.text.contains("Your subagent settled"));
    assert!(
        timed_report
            .text
            .contains(&format!("@{timed_name}: completed"))
    );

    let timed_digest_line = format!("@{timed_name}: completed");
    let parent_frame = capture_joined_until(
        &socket,
        &parent_pane_raw,
        |frame| {
            frame.contains("Type: SUBAGENT_REPORT")
                && frame.contains("From: @rimz")
                && frame.contains(&timed_digest_line)
        },
        CAPTURE_BUDGET,
    );
    assert!(
        parent_frame.contains("Type: SUBAGENT_REPORT")
            && parent_frame.contains("From: @rimz")
            && parent_frame.contains(&timed_digest_line)
            && !parent_frame.contains("stub done"),
        "parent pane did not receive status-only digests; timed report: {timed_report:?}\n{parent_frame}"
    );
    receive_and_finish(&timed_run);
}

#[test]
fn tmux_subagent_startup_deaths_below_the_cap_relaunch_and_report_completed() {
    let Some(child) = startup_death_child("2", "0") else {
        return;
    };
    assert_eq!(child.launches, 3, "two deaths, two relaunches");
    assert_eq!(child.run.status, rimz::store::run::RunStatus::Completed);
    let row = format!("@{}: completed", child.name);
    assert!(child.digest.contains(&row), "{}", child.digest);
    let response_path = rimz::harness::run::response_path(child.env.store().paths(), &child.run)
        .expect("response path");
    assert!(
        child
            .digest
            .contains(&format!("response: {}", response_path.display())),
        "{}",
        child.digest
    );
    assert!(child.parent_frame.contains(&row), "{}", child.parent_frame);
    assert_eq!(child.retries.len(), 2, "{:?}", child.retries);
    for (retry, expected) in child.retries.iter().zip(1u8..) {
        match retry {
            rimz::harness::assist_log::Assist::LaunchRetry {
                label,
                run_id,
                attempt,
                exit_code,
                relaunched,
                error,
                ..
            } => {
                assert_eq!(label, &format!("@{}", child.name));
                assert_eq!(run_id.as_ref(), Some(&child.run.run_id));
                assert_eq!(*attempt, expected);
                assert_eq!(*exit_code, Some(7));
                assert!(*relaunched && error.is_none());
            }
            other => panic!("expected a launch_retry assist: {other:?}"),
        }
    }
}

#[test]
fn tmux_subagent_reports_failed_when_every_relaunch_also_dies_at_startup() {
    let Some(child) = startup_death_child("9", "0") else {
        return;
    };
    assert_eq!(child.launches, 4, "the first launch and three relaunches");
    assert_eq!(child.run.status, rimz::store::run::RunStatus::Failed);
    let row = format!("@{}: failed", child.name);
    assert!(child.digest.contains(&row), "{}", child.digest);
    assert_eq!(
        child
            .digest
            .split_once("; ")
            .and_then(|(_, rest)| rest.split_once(", task:"))
            .map(|(reason, _)| reason),
        Some("4"),
        "the reason is the last attempt's output: {}",
        child.digest
    );
    assert!(child.parent_frame.contains(&row), "{}", child.parent_frame);
    assert_eq!(child.retries.len(), 3, "{:?}", child.retries);
}

#[test]
fn tmux_subagent_that_opened_a_session_is_not_relaunched() {
    let Some(child) = startup_death_child("0", "7") else {
        return;
    };
    assert_eq!(child.launches, 1, "an observed child fails as before");
    assert_eq!(child.run.status, rimz::store::run::RunStatus::Failed);
    assert!(
        child.digest.contains(&format!("@{}: failed", child.name)),
        "{}",
        child.digest
    );
    assert!(child.retries.is_empty(), "{:?}", child.retries);
}

struct StartupDeathChild {
    env: Env,
    name: String,
    run: rimz::store::run::RunRecord,
    /// The fleet digest queued for the parent.
    digest: String,
    /// The parent pane once that digest landed in it.
    parent_frame: String,
    /// How many times the provider stub was launched for the child.
    launches: usize,
    retries: Vec<rimz::harness::assist_log::Assist>,
}

/// Launch one `rimz subagents` child from a live parent, with the provider
/// stub dying before any hook on its first `startup_deaths` launches and
/// exiting `exit` after its hooks otherwise, and collect what the parent gets:
/// one run for the child, one fleet report, one row in it.
fn startup_death_child(startup_deaths: &str, exit: &str) -> Option<StartupDeathChild> {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return None;
    }
    let _rimz = rimz_bin()?;
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return None;
    }
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let stub_dir = write_hook_firing_agent(&env, "codex");
    let agent_path = path_with_front(&stub_dir);
    trust_agent_path(&env, "codex", &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let session = workspace_session(&env);
    let started = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "tmux", "start", "--no-attach"])
        .bounded_output_within(Duration::from_secs(45))
        .expect("start room");
    assert!(started.status.success(), "room start failed: {started:?}");
    let launch_pane = tmux_capture(
        &socket,
        &[
            "list-panes",
            "-t",
            &session,
            "-F",
            "#{pane_id}:#{pane_title}",
        ],
    )
    .lines()
    .find_map(|line| {
        let (pane, title) = line.split_once(':')?;
        (title != rimz::pane::SIDEBAR_CHROME_TITLE).then(|| pane.to_owned())
    })
    .expect("room shell pane");
    let parent_env = [
        ("RIMZ_TEST_AGENT_SESSION", "sess-retry-parent"),
        ("RIMZ_TEST_AGENT_WAIT_STDIN", "1"),
    ];
    for (key, value) in parent_env {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let parent = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &launch_pane)
        .envs(parent_env)
        .args([
            "--mux",
            "tmux",
            "agents",
            "codex",
            "coordinate the retry",
            "--name",
            "retry-parent",
            "--bg",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch parent");
    assert!(parent.status.success(), "parent launch failed: {parent:?}");
    let parent_agent = wait_for_named_agent(&env, "retry-parent", true, CAPTURE_BUDGET);
    let parent_launch_id = parent_agent.launch_id.clone().expect("parent launch id");
    let parent_pane = parent_agent
        .pane
        .as_ref()
        .expect("parent provider pane")
        .pane_id
        .raw()
        .to_owned();

    let launch_log = env.home_root.join("child-launches.log");
    let launch_log_arg = launch_log.display().to_string();
    let child_env = [
        ("RIMZ_TEST_AGENT_SESSION", "sess-retry-child"),
        ("RIMZ_TEST_AGENT_WAIT_STDIN", "0"),
        ("RIMZ_TEST_AGENT_LAUNCH_LOG", launch_log_arg.as_str()),
        ("RIMZ_TEST_AGENT_STARTUP_DEATHS", startup_deaths),
        ("RIMZ_TEST_AGENT_EXIT", exit),
    ];
    for (key, value) in child_env {
        tmux(&socket, &["set-environment", "-t", &session, key, value]);
    }
    let launched = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("TMUX_PANE", &parent_pane)
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            parent_launch_id.as_str(),
        )
        .envs(child_env)
        .args([
            "--mux",
            "tmux",
            "subagents",
            "codex",
            "survive startup",
            "--timeout",
            "2m",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch child");
    assert!(
        launched.status.success(),
        "child launch failed: {launched:?}"
    );
    let name = launched_subagent_name(&launched);
    let child_runs = || {
        rimz::harness::run::list(env.store().paths())
            .expect("read runs")
            .into_iter()
            .filter(|run| run.agent_name.as_deref() == Some(name.as_str()))
            .collect::<Vec<_>>()
    };
    let run = wait_for_named_terminal_run(&env, &name, CAPTURE_BUDGET);
    let reports = || {
        env.store()
            .list_messages()
            .expect("list fleet digest")
            .into_iter()
            .filter(|message| {
                matches!(
                    message.sender,
                    rimz::store::message::MessageSender::Harness {
                        notice: rimz::store::message::HarnessNotice::SubagentReport
                    }
                )
            })
            .map(|message| message.text)
            .collect::<Vec<_>>()
    };
    let deadline = Instant::now() + CAPTURE_BUDGET;
    while reports().is_empty() {
        assert!(Instant::now() < deadline, "fleet digest was not queued");
        std::thread::sleep(Duration::from_millis(25));
    }
    let row = format!("@{name}: ");
    let parent_frame = capture_joined_until(
        &socket,
        &parent_pane,
        |frame| frame.contains("Type: SUBAGENT_REPORT") && frame.contains(&row),
        CAPTURE_BUDGET,
    );
    let runs = child_runs();
    assert_eq!(runs.len(), 1, "every attempt shares one run: {runs:?}");
    let mut reports = reports();
    assert_eq!(reports.len(), 1, "the parent gets one report: {reports:?}");
    let digest = reports.remove(0);
    assert_eq!(digest.matches(&row).count(), 1, "one row: {digest}");
    let launches = std::fs::read_to_string(&launch_log)
        .expect("child launch log")
        .lines()
        .count();
    let retries = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None)
        .into_iter()
        .map(|record| record.assist)
        .filter(|assist| {
            matches!(
                assist,
                rimz::harness::assist_log::Assist::LaunchRetry { .. }
            )
        })
        .collect();
    Some(StartupDeathChild {
        run: rimz::harness::run::load(env.store().paths(), &run.run_id).expect("reload run"),
        env,
        name,
        digest,
        parent_frame,
        launches,
        retries,
    })
}

#[test]
fn tmux_completed_subagent_status_lingers_until_parent_pane_disappears() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
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
    trust_agent_path(&env, "codex", &agent_path);
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
    let child_name = launched_subagent_name(&child);

    let child_run = wait_for_named_terminal_run(&env, &child_name, CAPTURE_BUDGET);
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
        .find(|agent| agent.name.as_deref() == Some(child_name.as_str()))
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
            .any(|agent| agent.name.as_deref() == Some(child_name.as_str())),
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
            .any(|agent| agent.name.as_deref() == Some(child_name.as_str()));
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
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
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
    trust_agent_path(&env, "codex", &agent_path);
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

#[test]
fn tmux_standalone_account_room_launches_into_its_home_and_refuses_cross_account_resume() {
    account_room_journey("standalone");
}

#[test]
fn tmux_shared_account_room_resumes_under_the_rooms_new_account() {
    account_room_journey("shared");
}

/// Launch on a named Claude account, then rebirth the room on `default`: a
/// standalone account's session is refused, a shared one's continues there.
fn account_room_journey(history: &str) {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let work_home = env.home_root.join("claude-work");
    let added = env
        .rimz()
        .args(["accounts", "add", "claude", "work", "--history", history])
        .arg("--home")
        .arg(&work_home)
        .bounded_output_within(Duration::from_secs(30))
        .expect("add work account");
    assert!(
        added.status.success(),
        "accounts add failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let stub_dir = write_hook_firing_agent(&env, "claude");
    let shim = stub_dir.join("claude");
    let launched_homes = env.home_root.join("launched-homes");
    let body = std::fs::read_to_string(&shim).expect("read claude shim");
    // A real provider saves its conversation under its home; resume planning
    // reads that file once any home holds a `projects` directory.
    let body = body.replacen(
        "session=",
        &format!(
            "printf '%s\\n' \"${{CLAUDE_CONFIG_DIR:-native}}\" >> {}\n\
             saved=\"${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}/projects/$(printf '%s' \"$PWD\" | sed 's/[^A-Za-z0-9]/-/g')\"\n\
             mkdir -p \"$saved\"\n\
             printf '{{}}\\n' > \"$saved/${{RIMZ_TEST_AGENT_SESSION:-sess-hook-agent}}.jsonl\"\n\
             session=",
            shell_quote(&launched_homes.display().to_string())
        ),
        1,
    );
    std::fs::write(&shim, body).expect("write claude shim");
    let agent_path = path_with_front(&stub_dir);
    trust_agent_path(&env, "claude", &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());
    let session = workspace_session(&env);

    let started = env
        .rimz()
        .env("PATH", &agent_path)
        .env_remove("CLAUDE_CONFIG_DIR")
        .args([
            "--mux",
            "tmux",
            "start",
            "--no-attach",
            "--account",
            "claude=work",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("start work room");
    assert!(
        started.status.success(),
        "room start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    tmux(
        &socket,
        &[
            "set-environment",
            "-t",
            &session,
            "RIMZ_TEST_AGENT_WAIT_STDIN",
            "1",
        ],
    );
    let launched = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("RIMZ_TEST_AGENT_WAIT_STDIN", "1")
        .args([
            "--mux",
            "tmux",
            "agents",
            "claude",
            "work on the account",
            "--name",
            "account-worker",
            "--bg",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch work agent");
    assert!(
        launched.status.success(),
        "agent launch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&launched.stdout),
        String::from_utf8_lossy(&launched.stderr)
    );

    let agent = wait_for_named_agent(&env, "account-worker", true, CAPTURE_BUDGET);
    assert_eq!(
        agent.login.as_ref().map(|login| login.as_str()),
        Some("work"),
        "the session is stamped with the room's account"
    );
    assert_eq!(
        std::fs::read_to_string(&launched_homes).expect("launched homes trace"),
        format!("{}\n", work_home.display()),
        "the agent process runs under the work account home"
    );

    let restamp = env
        .rimz()
        .env("PATH", &agent_path)
        .args([
            "--mux",
            "tmux",
            "start",
            "--no-attach",
            "--account",
            "claude=default",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("start with another account");
    let stderr = String::from_utf8_lossy(&restamp.stderr);
    assert!(
        !restamp.status.success(),
        "a live room keeps its account: {stderr}"
    );
    assert!(
        stderr.contains("rimz accounts use claude default"),
        "{stderr}"
    );

    wait_for_live_roster_entry(&env, &agent, CAPTURE_BUDGET);
    let reset = env
        .rimz()
        .env("PATH", &agent_path)
        .env_remove("CLAUDE_CONFIG_DIR")
        .args(["--mux", "tmux", "reset", "--yes", "--no-start"])
        .bounded_output_within(Duration::from_secs(45))
        .expect("reset work room");
    assert!(
        reset.status.success(),
        "reset failed: {}",
        String::from_utf8_lossy(&reset.stderr)
    );
    // Nothing reads room state between the reset and this start: a probe here
    // delays the rebirth past the race a failing run loses.
    let reborn = env
        .rimz()
        .env("PATH", &agent_path)
        .env("RUST_LOG", REBIRTH_TRACE)
        .env_remove("CLAUDE_CONFIG_DIR")
        .args([
            "--mux",
            "tmux",
            "start",
            "--no-attach",
            "--account",
            "claude=default",
        ])
        .bounded_output_within(Duration::from_secs(45))
        .expect("start default room");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&reborn.stdout),
        String::from_utf8_lossy(&reborn.stderr)
    );
    assert!(reborn.status.success(), "rebirth failed: {output}");
    if history == "shared" {
        let expected = format!("{}\nnative\n", work_home.display());
        let deadline = Instant::now() + CAPTURE_BUDGET;
        let (homes, stamp) = loop {
            let homes = std::fs::read_to_string(&launched_homes).expect("launched homes trace");
            // The audit rollup keeps the row once the reopened stand-in exits.
            let stamp = env
                .store()
                .runtime_projection(rimz::RuntimeScope::Audit)
                .expect("read audit rollup")
                .agents
                .iter()
                .find(|agent| agent.name.as_deref() == Some("account-worker"))
                .map(|agent| agent.login.clone());
            if (homes == expected && stamp == Some(None)) || Instant::now() >= deadline {
                break (homes, stamp);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(
            homes,
            expected,
            "the work session is reopened under the default account: {output}\n{}",
            rebirth_evidence(&env, &socket, &agent)
        );
        assert_eq!(
            stamp,
            Some(None),
            "the stamp follows the account it now runs on"
        );
        return;
    }
    let mismatch = "@account-worker's session belongs to claude account `work`; this room now launches claude on `default`. Run `rimz accounts use claude work` to resume it, then switch back.";
    assert!(
        output.contains(mismatch) && output.contains("(different account)"),
        "rebirth warns and skips: {output}\n{}",
        rebirth_evidence(&env, &socket, &agent)
    );
    assert_eq!(
        std::fs::read_to_string(&launched_homes).expect("launched homes trace"),
        format!("{}\n", work_home.display()),
        "no work session is reopened under the default account"
    );

    let resume = || {
        env.rimz()
            .env("PATH", &agent_path)
            .env("TMUX", tmux_env(&socket))
            .env_remove("CLAUDE_CONFIG_DIR")
            .args(["--mux", "tmux", "agents", "resume", "#project"])
            .bounded_output_within(Duration::from_secs(45))
            .expect("resume the work lane")
    };
    let refused = resume();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "explicit resume refuses: {stderr}"
    );
    assert!(stderr.contains(mismatch), "{stderr}");

    let workspace = env.resolve_workspace(&env.project_root);
    let switched = env
        .rimz()
        .envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &workspace.project_root,
        ))
        .args(["accounts", "use", "claude", "work"])
        .bounded_output_within(Duration::from_secs(30))
        .expect("switch the room back to work");
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );
    let resumed = resume();
    assert!(
        resumed.status.success(),
        "same-account resume: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let expected = format!("{0}\n{0}\n", work_home.display());
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let homes = loop {
        let homes = std::fs::read_to_string(&launched_homes).expect("launched homes trace");
        if homes == expected || Instant::now() >= deadline {
            break homes;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        homes, expected,
        "after switching back, the work session resumes in its home"
    );
}

/// The account live-agent count sees a supervised headless run: its wrapper is
/// placed in a pane like every other launch, so a history switch that would
/// remove a link under it is refused, and proceeds once the process is dead.
#[test]
fn tmux_supervised_run_holds_its_account_history_link_until_it_dies() {
    if which::which("tmux").is_err() {
        crate::common::skip("tmux not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let work_home = declare_shared_work_account(&env);
    let stub_dir = write_hook_firing_agent(&env, "claude");
    let agent_path = path_with_front(&stub_dir);
    trust_agent_path(&env, "claude", &agent_path);
    let socket = managed_socket(&env.runtime_root);
    let _server = TmuxServerGuard::new(socket.clone());

    let launched = env
        .rimz()
        .env("PATH", &agent_path)
        .env("TMUX", tmux_env(&socket))
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "120000")
        .args(["--mux", "tmux"])
        .args(SUPERVISED_ACCOUNT_RUN)
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch supervised run");
    assert!(launched.status.success(), "launch failed: {launched:?}");
    let agent = wait_for_quiet_account_run(&env);

    assert_history_switch_follows_the_run(&env, &work_home, &agent);
}

/// The account run's row once the stub has fired its last hook before it
/// sleeps, so no hook write lands after the run is killed.
fn wait_for_quiet_account_run(env: &Env) -> rimz::agents::AgentState {
    let deadline = Instant::now() + CAPTURE_BUDGET;
    loop {
        let agent = wait_for_named_agent(env, "account-runner", true, CAPTURE_BUDGET);
        if agent.tool_calls.contains_key("apply_patch") {
            return agent;
        }
        assert!(
            Instant::now() < deadline,
            "the stub's tool hook never landed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Zellij: the same proof through a real Zellij server under the fixture's
/// runtime root.
#[test]
fn zellij_supervised_run_holds_its_account_history_link_until_it_dies() {
    if which::which("zellij").is_err() {
        crate::common::skip("zellij not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let work_home = declare_shared_work_account(&env);
    let stub_dir = write_hook_firing_agent(&env, "claude");
    let agent_path = path_with_front(&stub_dir);
    trust_agent_path(&env, "claude", &agent_path);
    // The run's pane inherits the server's environment, born here.
    let started = env
        .rimz()
        .env("PATH", &agent_path)
        .env("RIMZ_TEST_AGENT_SLEEP_MS", "120000")
        .args(["--mux", "zellij", "start", "--no-attach"])
        .bounded_output_within(Duration::from_secs(45))
        .expect("start room");
    assert!(started.status.success(), "room start failed: {started:?}");
    // A Zellij server lays out a new tab only for an attached client.
    let parser = Arc::new(Mutex::new(vt100::Parser::new(40, 160, 0)));
    let _client = AttachProcess::spawn(&workspace_session(&env), &parser, |cmd| {
        env.pin_pty_command(cmd);
    });

    let launched = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "zellij"])
        .args(SUPERVISED_ACCOUNT_RUN)
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch supervised run");
    assert!(launched.status.success(), "launch failed: {launched:?}");
    let agent = wait_for_quiet_account_run(&env);

    assert_history_switch_follows_the_run(&env, &work_home, &agent);
}

/// A sidebar that outlives its Zellij session is elected producer once the
/// dead elders' heartbeats lapse and sees an agent-less room in the pane cache.
/// The next birth must still park the agent the session died with.
#[cfg(target_os = "linux")]
#[test]
fn zellij_recovery_survives_a_sidebar_that_outlives_its_session() {
    if which::which("zellij").is_err() {
        crate::common::skip("zellij not on PATH");
        return;
    }
    let Some(_rimz) = rimz_bin() else {
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let stub_dir = write_hook_firing_agent(&env, "claude");
    let agent_path = path_with_front(&stub_dir);
    trust_agent_path(&env, "claude", &agent_path);
    // The launch runs the account check, which needs the login's hooks.
    env.install_agent_hooks("claude");
    let start = || {
        env.rimz()
            .env("PATH", &agent_path)
            .env("RIMZ_TEST_AGENT_SLEEP_MS", "120000")
            .args(["--mux", "zellij", "start", "--no-attach"])
            .bounded_output_within(Duration::from_secs(45))
            .expect("start room")
    };
    let started = start();
    assert!(started.status.success(), "room start failed: {started:?}");
    let session = workspace_session(&env);
    let parser = Arc::new(Mutex::new(vt100::Parser::new(40, 160, 0)));
    let client = AttachProcess::spawn(&session, &parser, |cmd| {
        env.pin_pty_command(cmd);
    });
    // A Zellij server lays out a new tab only for a client that has attached.
    let deadline = Instant::now() + CAPTURE_BUDGET;
    while parser
        .lock()
        .expect("parser")
        .screen()
        .contents()
        .trim()
        .is_empty()
    {
        assert!(Instant::now() < deadline, "the client never painted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let launched = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "zellij", "agents", "claude", "hold the room"])
        .args(["--name", "roster-worker", "--bg"])
        .bounded_output_within(Duration::from_secs(45))
        .expect("launch agent");
    assert!(launched.status.success(), "launch failed: {launched:?}");
    let agent = wait_for_named_agent(&env, "roster-worker", true, CAPTURE_BUDGET);
    wait_for_live_roster_entry(&env, &agent, CAPTURE_BUDGET);

    // The survivor: one of the room's own sidebars, started again outside it.
    let (argv, environ) = std::fs::read_dir("/proc")
        .expect("read /proc")
        .filter_map(|entry| {
            let dir = entry.ok()?.path();
            let nul_separated = |name: &str| {
                let bytes = std::fs::read(dir.join(name)).ok()?;
                let text = String::from_utf8(bytes).ok()?;
                Some(
                    text.split_terminator('\0')
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                )
            };
            Some((nul_separated("cmdline")?, nul_separated("environ")?))
        })
        .find(|(argv, environ)| {
            argv.contains(&session)
                && argv.windows(2).any(|pair| pair == ["sidebar", "serve"])
                && !environ
                    .iter()
                    .any(|var| var.starts_with("RIMZ_SIDEBAR_WORKER="))
        })
        .expect("a sidebar supervisor of the room");
    let mut survivor = CommandBuilder::from_argv(argv.iter().map(Into::into).collect());
    survivor.env_clear();
    for var in &environ {
        if let Some((key, value)) = var.split_once('=')
            && key != "RIMZ_SIDEBAR_INSTANCE_ID"
        {
            survivor.env(key, value);
        }
    }
    survivor.cwd(&env.project_root);
    let _survivor = AttachProcess::on_pty(survivor, &parser);

    ZellijBackend::with_runtime_dir(&env.runtime_root)
        .kill_session(&session)
        .expect("end the session through its own socket");
    drop(client);
    let roster = env.store().paths().live_roster.clone();
    let deadline = Instant::now() + CAPTURE_BUDGET;
    let survivor_published = || {
        let emptied = std::fs::read(&roster)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|roster| roster["agents"] == serde_json::json!([]));
        emptied
            || env
                .diag_records(&session)
                .iter()
                .any(|record| matches!(record.event, DiagEvent::LiveRosterHeld { .. }))
    };
    while !survivor_published() {
        assert!(
            Instant::now() < deadline,
            "the survivor never reached a roster publication:\n{}",
            env.diag_tail(&session, DIAG_EVIDENCE_RECORDS)
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let reborn = start();
    let stderr = String::from_utf8_lossy(&reborn.stderr);
    assert!(reborn.status.success(), "rebirth failed: {stderr}");
    assert!(
        stderr.contains("previous session ended with agents still running"),
        "the birth read a roster without the lost agent: {stderr}\n{}",
        env.diag_tail(&session, DIAG_EVIDENCE_RECORDS)
    );
    // A clientless rebirth may not lay out the resumed tab in time; the agent
    // then stays parked, which recovers it just the same.
    if stderr.contains("rimz: resumed 1 agent") {
        return;
    }
    assert!(
        stderr.contains("its agents stay pending for a later rebirth or explicit resume"),
        "the recovered agent was neither resumed nor left pending: {stderr}"
    );
    let pending: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&env.store().paths().pending_recovery).expect("pending-recovery record"),
    )
    .expect("pending-recovery JSON");
    assert!(
        pending["agents"]
            .as_array()
            .is_some_and(|agents| agents.contains(&serde_json::json!([agent.kind, agent.agent_id]))),
        "the unopened tab's agent is not parked: {pending}\n{stderr}"
    );
}

const SUPERVISED_ACCOUNT_RUN: [&str; 10] = [
    "agents",
    "claude",
    "hold the account",
    "--name",
    "account-runner",
    "-p",
    "--bg",
    "--keep",
    "--timeout",
    "3m",
];

/// Declare a shared Claude account `work` under a fixture home and make it the
/// default for new rooms, before any provider has written to either home.
fn declare_shared_work_account(env: &Env) -> PathBuf {
    let work_home = env.home_root.join("claude-work");
    let mut add = env.rimz();
    add.args(["accounts", "add", "claude", "work", "--history", "shared"])
        .arg("--home")
        .arg(&work_home);
    let mut default = env.rimz();
    default.args(["accounts", "use", "--global", "claude", "work"]);
    for mut command in [add, default] {
        let out = command
            .bounded_output_within(Duration::from_secs(30))
            .expect("declare work account");
        assert!(out.status.success(), "declare work account: {out:?}");
    }
    work_home
}

/// Make the room publish a rollup that holds `agent`, which the count then
/// serves as long as nothing else writes. A publish runs at most once a
/// second, so the write that triggers it waits past that interval; a write
/// that still lands inside it leaves the rollup behind the log, so write
/// again until the rollup is seen fresh.
fn publish_room_rollup_with(env: &Env, agent: &rimz::ids::AgentSessionId) {
    let store = env.store();
    let deadline = Instant::now() + CAPTURE_BUDGET;
    loop {
        std::thread::sleep(Duration::from_millis(1_100));
        let emitted = env
            .rimz()
            .args(["events", "emit", "account.check"])
            .bounded_output_within(Duration::from_secs(30))
            .expect("emit a signal into the room");
        assert!(emitted.status.success(), "emit failed: {emitted:?}");
        let settled = Instant::now() + Duration::from_secs(2);
        while Instant::now() < settled {
            if rimz::store::snapshot::read_fresh_latest(store.paths())
                .is_some_and(|rollup| rollup.agents.iter().any(|row| row.agent_id == *agent))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            Instant::now() < deadline,
            "the room never published a fresh rollup"
        );
    }
}

/// With `agent` live on the shared `work` account, a switch to standalone
/// history is refused and changes nothing; once the agent's processes are
/// dead, with no hook or settle to tell the room, the same switch succeeds.
fn assert_history_switch_follows_the_run(
    env: &Env,
    work_home: &Path,
    agent: &rimz::agents::AgentState,
) {
    assert_eq!(
        agent.login.as_ref().map(|login| login.as_str()),
        Some("work"),
        "the run is stamped with the account"
    );
    assert!(agent.pane.is_some(), "the run's row has a pane");
    let config = env.rimz_home().join("config.toml");
    let declared = std::fs::read_to_string(&config).expect("read machine config");
    let link = work_home.join("projects");
    assert!(link.is_symlink(), "shared history links `projects`");
    let switch = || {
        env.rimz()
            .args([
                "accounts",
                "add",
                "claude",
                "work",
                "--history",
                "standalone",
            ])
            .bounded_output_within(Duration::from_secs(30))
            .expect("switch work account to standalone")
    };

    let refused = switch();
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a live run refuses the switch: {refused:?}"
    );
    assert!(
        error.contains("cannot unlink `projects`") && error.contains("1 live agent(s)"),
        "{error}"
    );
    assert!(link.is_symlink(), "a refused switch leaves the link");
    assert_eq!(
        std::fs::read_to_string(&config).expect("read machine config"),
        declared,
        "a refused switch leaves the config as it was"
    );

    publish_room_rollup_with(env, &agent.agent_id);
    // Kill the wrapper with its provider, so nothing settles the run and the
    // rollup the room last published still says the agent is live.
    let owner = agent.runtime_owner.as_ref().expect("provider owner");
    let provider = owner.pid;
    let wrapper = rimz::proc::comm_and_ppid(provider)
        .map(|(_, ppid)| ppid)
        .expect("provider parent process");
    // The wrapper's death hangs up the pane, which can take the provider and
    // have it reaped before its own signal is sent; only the wrapper's kill
    // must find its process.
    for (pid, must_exist) in [(wrapper, true), (provider, false)] {
        let killed = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .bounded_output()
            .expect("kill the run's wrapper and provider");
        assert!(
            killed.status.success() || !must_exist,
            "kill run: {killed:?}"
        );
    }
    // Wait on the liveness the account count reads, which a zombie awaiting
    // its reaper already fails.
    let deadline = Instant::now() + CAPTURE_BUDGET;
    while rimz::proc::process_is_live(provider, owner.process_start.as_deref()) {
        assert!(Instant::now() < deadline, "provider did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }

    let switched = switch();
    assert!(
        switched.status.success(),
        "a dead run no longer holds the link: {}",
        String::from_utf8_lossy(&switched.stderr)
    );
    assert!(!link.is_symlink(), "the switch removed the link");
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
            panic!(
                "timed out waiting for agent {name}; agents: {agents:?}\npanes:\n{}",
                tmux_screens(&managed_socket(&env.runtime_root)),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every pane's screen on the tmux server at `socket`, for a timeout message:
/// a provisional agent's pane shows whether its shim ran and what the
/// `SessionStart` feed printed.
fn tmux_screens(socket: &Path) -> String {
    let tmux = |args: &[&str]| {
        Command::new("tmux")
            .scrub_session_env()
            .arg("-S")
            .arg(socket)
            .args(args)
            .bounded_output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_else(|err| format!("tmux {args:?}: {err}"))
    };
    tmux(&["list-panes", "-a", "-F", "#{pane_id}"])
        .lines()
        .map(|pane| {
            format!(
                "--- {pane}\n{}",
                tmux(&["capture-pane", "-p", "-J", "-t", pane])
            )
        })
        .collect()
}

/// The `RUST_LOG` filter that makes a `rimz start` print why its rebirth left
/// an agent out of recovery.
const REBIRTH_TRACE: &str = "warn,rimz::harness::rebirth=debug,rimz::cli::room=debug";

/// What the room holds after a rebirth that did not resume `agent`, for a
/// failure message beside the start's own decision trace: the audit row and
/// whether its owner still runs, the roster, the death marker (it lists the
/// agent when the roster named it), the agent's events from each archived and
/// the active log, and the tmux sessions.
fn rebirth_evidence(env: &Env, socket: &Path, agent: &rimz::agents::AgentState) -> String {
    let store = env.store();
    let row = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .map(|projection| {
            projection
                .agents
                .iter()
                .find(|row| row.agent_id == agent.agent_id)
                .map(|row| {
                    format!(
                        "ended_at={:?} owner={:?} liveness={:?}",
                        row.ended_at,
                        row.runtime_owner,
                        rimz::store::runtime::agent_liveness(row)
                    )
                })
        });
    let file = |path: &Path| {
        std::fs::read_to_string(path).unwrap_or_else(|err| format!("unreadable: {err}"))
    };
    // The reset rotated the log the agent's launch and teardown stamps were
    // written to, so the archives are read with the active log.
    let mut logs = std::fs::read_dir(&store.paths().events_archive_dir)
        .map(|dir| dir.flatten().map(|entry| entry.path()).collect::<Vec<_>>())
        .unwrap_or_default();
    logs.sort();
    logs.push(store.paths().events_log.clone());
    let events = logs
        .iter()
        .map(|log| {
            let name = log.file_name().unwrap_or_default().to_string_lossy();
            let label = if *log == store.paths().events_log {
                format!("active/{name}")
            } else {
                format!("archive/{name}")
            };
            match rimz::store::event_log::read_all(log) {
                Ok(events) => events
                    .iter()
                    .filter(|event| event.params.get().contains(agent.agent_id.as_str()))
                    .map(|event| {
                        format!(
                            "\n  [{label}] {} {} {} {}",
                            event.timestamp, event.source, event.method, event.params
                        )
                    })
                    .collect(),
                Err(err) => format!("\n  [{label}] unreadable: {err}"),
            }
        })
        .collect::<String>();
    let sessions = Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(socket)
        .arg("list-sessions")
        .bounded_output()
        .map(|out| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
        .unwrap_or_else(|err| format!("tmux list-sessions: {err}"));
    format!(
        "after the rebirth:\naudit row: {row:?}\nlive roster: {}\nlast death: {}\nevents naming the agent:{events}\ntmux sessions: {sessions}",
        file(&store.paths().live_roster),
        file(&store.paths().last_death_marker),
    )
}

/// A reset rotates the event log before the rebirth runs, so the stamps
/// written up to and during the teardown are in the archive by the time a
/// failing journey reads its evidence.
#[test]
fn tmux_rebirth_evidence_keeps_the_events_a_reset_rotated_out() {
    let Some(_rimz) = rimz_bin() else {
        return;
    };
    let env = Env::new();
    let fed = env.run_hook(
        "claude",
        &session_start_at(
            "sess-rotated",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        )
        .to_string(),
    );
    assert!(
        fed.status.success(),
        "hook feed failed: {}",
        String::from_utf8_lossy(&fed.stderr)
    );
    let store = env.store();
    let agent = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("read audit rollup")
        .agents
        .into_iter()
        .find(|agent| agent.agent_id.as_str() == "sess-rotated")
        .expect("the hook registered the agent");
    store.reset_records(false).expect("rotate the event log");

    let evidence = rebirth_evidence(&env, &managed_socket(&env.runtime_root), &agent);
    let stamp = evidence
        .lines()
        .find(|line| line.contains("agent.lifecycle") && line.contains("SessionStart"))
        .unwrap_or_else(|| panic!("the pre-reset stamp is missing: {evidence}"));
    assert!(stamp.contains("[archive/events."), "{stamp}");
    assert!(
        stamp.contains(" claude "),
        "the event keeps its source: {stamp}"
    );
}

/// Rebirth recovers the sessions the sidebar producer last published, which
/// trails the store's bind, so a teardown that expects a resume waits for it.
fn wait_for_live_roster_entry(env: &Env, agent: &rimz::agents::AgentState, budget: Duration) {
    let path = env.store().paths().live_roster.clone();
    let entry = serde_json::to_value((&agent.kind, &agent.agent_id)).expect("encode roster entry");
    let deadline = Instant::now() + budget;
    loop {
        let roster = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let agents = roster
            .as_ref()
            .and_then(|roster| roster["agents"].as_array());
        if agents.is_some_and(|agents| agents.contains(&entry)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the sidebar producer never published {entry} to {}: {roster:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_named_run(env: &Env, name: &str, budget: Duration) -> rimz::store::run::RunRecord {
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
) -> rimz::store::run::RunRecord {
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
///
/// A loaded Zellij server can turn an attach away with "No session with the
/// name ... found" while the session is alive, so an exited client is
/// re-attached on the next read, as the backend suite's attached client does.
struct AttachedZellijScreen {
    namespace: PathBuf,
    session: String,
    parser: Arc<Mutex<vt100::Parser>>,
    client: AttachProcess,
}

struct AttachProcess {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl AttachedZellijScreen {
    fn new(namespace: &ZellijNamespace, session: &str, cols: u16, rows: u16) -> Self {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let path = namespace.path();
        let client = AttachProcess::spawn(session, &parser, |cmd| {
            ZellijNamespace::pin_pty_at(path, cmd);
        });
        Self {
            namespace: namespace.path().to_path_buf(),
            session: session.to_owned(),
            parser,
            client,
        }
    }

    fn contents(&mut self) -> String {
        if matches!(self.client.child.try_wait(), Ok(Some(_))) {
            self.reattach();
        }
        self.parser.lock().expect("parser").screen().contents()
    }

    fn reattach(&mut self) {
        self.client.stop();
        let (rows, cols) = self.parser.lock().expect("parser").screen().size();
        *self.parser.lock().expect("parser") = vt100::Parser::new(rows, cols, 0);
        let path = &self.namespace;
        self.client = AttachProcess::spawn(&self.session, &self.parser, |cmd| {
            ZellijNamespace::pin_pty_at(path, cmd);
        });
    }

    fn wait_until(&mut self, mut ready: impl FnMut(&str) -> bool, budget: Duration) -> String {
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

impl AttachProcess {
    /// A `zellij attach` client on a PTY, its server reached through `pin`.
    fn spawn(
        session: &str,
        parser: &Arc<Mutex<vt100::Parser>>,
        pin: impl FnOnce(&mut CommandBuilder),
    ) -> Self {
        let mut cmd = CommandBuilder::new("zellij");
        cmd.args(["attach", session]);
        pin(&mut cmd);
        Self::on_pty(cmd, parser)
    }

    /// `cmd` on a PTY this test owns, so its room's death does not hang it up.
    fn on_pty(cmd: CommandBuilder, parser: &Arc<Mutex<vt100::Parser>>) -> Self {
        let (rows, cols) = parser.lock().expect("parser").screen().size();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let child = pair.slave.spawn_command(cmd).expect("spawn on pty");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let sink = Arc::clone(parser);
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
            reader: Some(reader),
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop(self.master.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for AttachProcess {
    fn drop(&mut self) {
        self.stop();
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
        // A session under load can fail a listing or answer it with empty
        // stdout while it is alive; retry within the budget like any poll.
        let values = output
            .status
            .success()
            .then(|| serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout).ok())
            .flatten();
        let Some(mut values) = values else {
            assert!(
                Instant::now() < deadline,
                "list-panes never answered for {session}: {}; stdout: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
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
        produced_at_ms: rimz::utils::time::unix_now_ms(),
        writer: None,
        focused_pane: None,
        clients: None,
        panes,
    };
    rimz::mux::zellij::pane_topology::write_pane_topology_cache(runtime, &cache)
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

/// The width of the frame the renderer painted, read off its right-aligned
/// footer. Only a footer beside the pane's right border counts: after a resize
/// and before the renderer repaints, Zellij shows the old frame rewrapped (a
/// shrink leaves the footer's tail mid-row) or unpadded (a grow leaves it short
/// of the border), and neither is a width the sidebar chose.
fn rendered_sidebar_width(screen: &str) -> Option<usize> {
    const FOOTER: &str = "? for help";
    screen
        .lines()
        .filter_map(|line| line.split_once(FOOTER))
        .find(|(_, rest)| rest.strip_prefix(' ').unwrap_or(rest).starts_with('│'))
        .map(|(prefix, _)| prefix.chars().count() + FOOTER.chars().count())
}

fn wait_for_rendered_sidebar_width(
    client: &mut AttachedZellijScreen,
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
    env.resolve_workspace(&env.project_root).session_name
}

fn tmux_env(socket: &Path) -> String {
    format!("{},0,0", socket.display())
}

fn trust_codex_hooks(env: &Env) {
    let config = env.agent_config_path("codex");
    let mut text = std::fs::read_to_string(&config).expect("read codex config");
    text.push_str(&format!(
        "\n[projects.{}]\ntrust_level = \"trusted\"\n",
        toml::Value::String(env.project_root.display().to_string()),
    ));
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

fn trust_agent_path(env: &Env, agent: &'static str, path: &OsStr) {
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
            name: agent,
            env: std::collections::BTreeMap::from([("PATH", path.to_string_lossy().into_owned())]),
        }],
    })
    .expect("serialize trusted agent PATH config");
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

fn launched_subagent_name(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .expect("subagent launch prints its minted petname")
        .to_owned()
}

fn live_agent_pane_fixture(
    agent: &rimz::agents::AgentState,
    parent_pane: &rimz::pane::PaneRef,
) -> rimz::pane::PaneRef {
    let mut pane = agent.pane.clone().expect("agent provider pane");
    pane.session_name.clone_from(&parent_pane.session_name);
    pane.view_id.clone_from(&parent_pane.view_id);
    pane.view_kind = parent_pane.view_kind;
    pane.command = Some(agent.kind.to_string());
    pane.spawn_command = None;
    pane.cwd.clone_from(&agent.worktree_path);
    pane.pane_pid = None;
    pane.pane_process_start = None;
    pane.hosted_agent_kind = Some(agent.kind.clone());
    pane.hosted_agent_process_start = None;
    pane
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
    capture_until_with_join(socket, session, pred, budget, false)
}

fn capture_joined_until(
    socket: &Path,
    session: &str,
    pred: impl Fn(&str) -> bool,
    budget: Duration,
) -> String {
    capture_until_with_join(socket, session, pred, budget, true)
}

fn capture_until_with_join(
    socket: &Path,
    session: &str,
    pred: impl Fn(&str) -> bool,
    budget: Duration,
    join_wrapped: bool,
) -> String {
    let deadline = Instant::now() + budget;
    let mut last = String::new();
    loop {
        let mut command = Command::new("tmux");
        command
            .scrub_session_env()
            .arg("-S")
            .arg(socket)
            .args(["capture-pane", "-p"]);
        if join_wrapped {
            // Digest recipients can shrink while terminal children await receipt.
            command.args(["-J", "-S", "-"]);
        }
        let out = command
            .args(["-t", session])
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
