//! Deep end-to-end smokes: the *real* sidebar pane in a *real* multiplexer.
//!
//! The phase journey (`sidebar_phases.rs`) drives the renderer through a
//! `portable-pty`; these two tests close the loop by birthing a real session
//! with a real `rimz-sidebar` pane, firing an agent hook, and capturing what
//! the actual mux pane shows. They self-skip without the mux binary (the
//! common CI shape) and under a socket-bind sandbox.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

use super::{rimz_sidebar_bin, session_start_at};
use crate::common::Env;

const CAPTURE_BUDGET: Duration = Duration::from_secs(15);

/// Shell line that runs the renderer over `env`'s ledger but with its own short
/// `XDG_RUNTIME_DIR` (the wakeup socket must stay under the AF_UNIX limit).
fn sidebar_serve_line(
    env: &Env,
    sidebar: &Path,
    runtime: &Path,
    mux: &str,
    session: &str,
) -> String {
    format!(
        "XDG_STATE_HOME={state} XDG_CONFIG_HOME={config} XDG_RUNTIME_DIR={runtime} HOME={home} \
         RIMZ_BIN={rimz} exec {sidebar} serve --mux {mux} --workspace-id {ws} \
         --session-name {session} --tick-seconds 1",
        state = env.state_root().display(),
        config = env.config_root().display(),
        runtime = runtime.display(),
        home = env.project_root.display(),
        rimz = env.rimz_bin().display(),
        sidebar = sidebar.display(),
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

/// tmux: split a real `rimz-sidebar` pane beside a live command, fire `codex
/// SessionStart`, and capture the sidebar pane until the agent row appears.
#[test]
fn tmux_room_shows_agent_after_hook() {
    if which::which("tmux").is_err() {
        eprintln!("tmux not on PATH; skipping deep tmux smoke");
        return;
    }
    let Some(sidebar) = rimz_sidebar_bin() else {
        eprintln!("rimz-sidebar not built; skipping deep tmux smoke");
        return;
    };
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let server_dir = TempDir::new().expect("tmux socket dir");
    let socket = server_dir.path().join("tmux.sock");
    let runtime = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("short runtime dir");
    let _server = TmuxServerGuard {
        socket: socket.clone(),
    };
    let fake_codex = fake_codex_bin(server_dir.path());

    // A session with a foreground agent-shaped command, then a sidebar pane
    // beside it. The hook still drives ledger identity; the live pane list
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
    let serve = sidebar_serve_line(&env, &sidebar, runtime.path(), "tmux", "room");
    tmux(&socket, &["split-window", "-h", "-t", "room", &serve]);

    // Wire codex the way the user does, then run it through its installed
    // hook against the shared ledger — the only way a real agent reaches Rimz.
    env.install_agent_hooks("codex");
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
        &[("TMUX_PANE", &codex_pane)],
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
        |s| s.contains("codex") && s.contains("main"),
        CAPTURE_BUDGET,
    );
    assert!(
        screen.contains("codex"),
        "the live tmux sidebar pane should show the agent row:\n{screen}"
    );
    assert!(
        screen.contains("main"),
        "the agent should appear under its worktree group:\n{screen}"
    );
}

/// Zellij: same arc through a real Zellij session. Self-skips without `zellij`.
#[test]
fn zellij_room_shows_agent_after_hook() {
    if which::which("zellij").is_err() {
        eprintln!("zellij not on PATH; skipping deep zellij smoke");
        return;
    }
    let Some(sidebar) = rimz_sidebar_bin() else {
        eprintln!("rimz-sidebar not built; skipping deep zellij smoke");
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
    let cwd = TempDir::new().expect("cwd dir");
    let fake_codex = fake_codex_bin(cwd.path());
    let name = "rimz-deep-zellij";
    let _cleanup = ZellijSessionGuard {
        name: name.to_owned(),
        runtime: runtime.path().to_path_buf(),
    };

    // Birth a background session whose left pane is a real renderer over the
    // shared ledger (the self-close layout shape from `backend/zellij.rs`).
    let serve = sidebar_serve_line(&env, &sidebar, runtime.path(), "zellij", name);
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
    let created = scoped_zellij(runtime.path())
        .args(["attach", "--create-background", name, "options"])
        .arg("--default-cwd")
        .arg(&env.project_root)
        .arg("--default-layout")
        .arg(&layout_path)
        .status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    env.install_agent_hooks("codex");
    let out = env.run_installed_hook(
        "codex",
        &session_start_at(
            "sess-1",
            "GPT-5.5",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        )
        .to_string(),
    );
    assert!(
        out.status.success(),
        "codex hook failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Attach a real client so panes get a usable size and render (a
    // never-attached background session draws into a placeholder pane). Read
    // the composited screen the user would see and look for the agent row.
    let screen = attach_and_read_until(runtime.path(), name, "codex", CAPTURE_BUDGET);
    assert!(
        screen.contains("codex"),
        "the live zellij sidebar pane should show the agent row:\n{screen}"
    );
}

/// Attach a `portable-pty` client to `session` and poll the composited screen
/// (vt100-parsed master output) until it contains `needle` or the budget
/// elapses.
fn attach_and_read_until(runtime: &Path, session: &str, needle: &str, budget: Duration) -> String {
    const ROWS: u16 = 40;
    const COLS: u16 = 120;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new("zellij");
    cmd.args(["attach", session]);
    cmd.env("XDG_RUNTIME_DIR", runtime);
    let mut child = pair.slave.spawn_command(cmd).expect("attach zellij");
    drop(pair.slave);

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let sink = Arc::clone(&buf);
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => sink.lock().expect("sink").extend_from_slice(&chunk[..n]),
            }
        }
    });

    let deadline = Instant::now() + budget;
    let mut text = String::new();
    while Instant::now() < deadline {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(&buf.lock().expect("buf"));
        text = parser.screen().contents();
        if text.contains(needle) {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = reader.join();
    text
}

// --- tmux helpers ---

fn tmux(socket: &Path, args: &[&str]) {
    tmux_capture(socket, args);
}

/// Run a tmux command and return its trimmed stdout (used to read a pane id).
fn tmux_capture(socket: &Path, args: &[&str]) -> String {
    let out = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(args)
        .output()
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
            .arg("-S")
            .arg(socket)
            .args(["capture-pane", "-p", "-t", session])
            .output()
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

struct TmuxServerGuard {
    socket: std::path::PathBuf,
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}

// --- zellij helpers ---

fn scoped_zellij(runtime: &Path) -> Command {
    let mut cmd = Command::new("zellij");
    cmd.env("XDG_RUNTIME_DIR", runtime);
    cmd
}

struct ZellijSessionGuard {
    name: String,
    runtime: std::path::PathBuf,
}

impl Drop for ZellijSessionGuard {
    fn drop(&mut self) {
        let _ = scoped_zellij(&self.runtime)
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}
