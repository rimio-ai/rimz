//! Live regression tests for the one-root/one-backend room invariant.

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rimz::workspace::WorkspaceResolver;

use crate::common::{CommandTimeoutExt, Env, ScrubSessionEnvExt};

#[test]
fn start_refuses_when_rival_backend_runs_room() {
    let Some(room) = TmuxRoom::start() else {
        return;
    };

    let output = room
        .rimz()
        .args(["--mux", "zellij", "start"])
        .bounded_output()
        .expect("run rival zellij start");

    assert!(
        !output.status.success(),
        "rival zellij start should fail: {:?}",
        output.status,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&room.session_name),
        "stderr should name the session, got: {stderr}",
    );
    assert!(
        stderr.contains("tmux") && stderr.contains("zellij"),
        "stderr should name both backends, got: {stderr}",
    );
    assert!(
        room.tmux_sessions().contains(&room.session_name),
        "refusal must leave the tmux room live",
    );
}

#[test]
fn attach_from_cwd_uses_live_backend_over_ambient_backend() {
    let Some(room) = TmuxRoom::start() else {
        return;
    };

    let output = room
        .rimz()
        .arg("attach")
        .env("ZELLIJ", "1")
        .bounded_output()
        .expect("run attach from cwd");

    assert!(
        output.status.success(),
        "attach from cwd should print successfully: {:?}",
        output.status,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tmux attach") && stdout.contains(&room.session_name),
        "attach should target the live tmux room, got: {stdout}",
    );
    assert!(
        !stdout.contains("zellij"),
        "attach should not follow the ambient zellij env, got: {stdout}",
    );
}

#[test]
fn start_auto_attaches_to_live_zellij_room() {
    let Some(room) = ZellijRoom::start() else {
        return;
    };

    let output = room
        .rimz()
        .arg("start")
        .bounded_output()
        .expect("run auto start");

    assert!(
        output.status.success(),
        "auto start should print successfully: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("zellij attach") && stdout.contains(&room.session_name),
        "auto start should target the live zellij room, got: {stdout}",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("already running under"),
        "auto start should not report a rival backend, got: {stderr}",
    );
}

#[test]
fn reset_targets_live_backend_and_rebirths_on_default() {
    let Some(room) = ZellijRoom::start() else {
        return;
    };

    let output = room
        .rimz()
        .args(["reset", "--yes"])
        .bounded_output()
        .expect("run auto reset");

    assert!(
        output.status.success(),
        "auto reset should succeed: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("already running under"),
        "auto reset should not report a rival backend, got: {stderr}",
    );
    assert!(
        !room.zellij_sessions().contains(&room.session_name),
        "reset should tear down the live zellij room",
    );
    assert!(
        room.tmux_sessions().contains(&room.session_name),
        "reset should rebirth on the tmux default",
    );
}

struct TmuxRoom {
    env: Env,
    session_name: String,
    tmux_tmpdir: PathBuf,
}

impl TmuxRoom {
    fn start() -> Option<Self> {
        if which::which("tmux").is_err() {
            eprintln!("tmux not on PATH; skipping single-backend room test");
            return None;
        }
        let env = Env::new();
        let tmux_tmpdir = env.project_root.join("tmux");
        std::fs::create_dir_all(&tmux_tmpdir).expect("mkdir tmux tmpdir");
        let workspace =
            WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
        let session_name = workspace.session_name;

        let output = {
            let mut cmd = env.rimz();
            cmd.args(["--mux", "tmux", "start"])
                .env("TMUX_TMPDIR", &tmux_tmpdir)
                .bounded_output()
                .expect("run tmux start")
        };
        assert!(
            output.status.success(),
            "tmux start failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );

        Some(Self {
            env,
            session_name,
            tmux_tmpdir,
        })
    }

    fn rimz(&self) -> Command {
        let mut cmd = self.env.rimz();
        cmd.env("TMUX_TMPDIR", &self.tmux_tmpdir);
        cmd
    }

    fn tmux_sessions(&self) -> Vec<String> {
        let output = tmux_output(
            &self.tmux_tmpdir,
            &["list-sessions", "-F", "#{session_name}"],
        );
        assert!(
            output.status.success(),
            "tmux list-sessions failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }
}

impl Drop for TmuxRoom {
    fn drop(&mut self) {
        let _ = tmux_output(&self.tmux_tmpdir, &["kill-server"]);
    }
}

struct ZellijRoom {
    env: Env,
    session_name: String,
    tmux_tmpdir: PathBuf,
}

impl ZellijRoom {
    fn start() -> Option<Self> {
        if which::which("zellij").is_err() {
            eprintln!("zellij not on PATH; skipping zellij auto-backend room test");
            return None;
        }
        if which::which("tmux").is_err() {
            eprintln!("tmux not on PATH; skipping zellij auto-backend room test");
            return None;
        }
        let env = Env::new();
        let tmux_tmpdir = env.project_root.join("tmux");
        std::fs::create_dir_all(&tmux_tmpdir).expect("mkdir tmux tmpdir");
        let workspace =
            WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
        let session_name = workspace.session_name;

        let output = {
            let mut cmd = env.rimz();
            cmd.args(["--mux", "zellij", "start"])
                .env("TMUX_TMPDIR", &tmux_tmpdir)
                .bounded_output()
                .expect("run zellij start")
        };
        assert!(
            output.status.success(),
            "zellij start failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );

        Some(Self {
            env,
            session_name,
            tmux_tmpdir,
        })
    }

    fn rimz(&self) -> Command {
        let mut cmd = self.env.rimz();
        cmd.env("TMUX_TMPDIR", &self.tmux_tmpdir);
        cmd
    }

    fn zellij_sessions(&self) -> Vec<String> {
        let output = self
            .zellij()
            .args(["list-sessions", "--no-formatting"])
            .bounded_output()
            .expect("list zellij sessions");
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stdout.contains("No active zellij sessions found")
                || stderr.contains("No active zellij sessions found")
            {
                return Vec::new();
            }
        }
        assert!(
            output.status.success(),
            "zellij list-sessions failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(live_zellij_session_name)
            .collect()
    }

    fn tmux_sessions(&self) -> Vec<String> {
        let output = tmux_output(
            &self.tmux_tmpdir,
            &["list-sessions", "-F", "#{session_name}"],
        );
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("error connecting") {
                return Vec::new();
            }
        }
        assert!(
            output.status.success(),
            "tmux list-sessions failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    fn zellij(&self) -> Command {
        let mut cmd = Command::new("zellij");
        cmd.scrub_session_env()
            .env("XDG_RUNTIME_DIR", &self.env.runtime_root)
            .env("XDG_STATE_HOME", self.env.state_root())
            .env("XDG_CONFIG_HOME", self.env.config_root())
            .env("XDG_CACHE_HOME", &self.env.home_root)
            .env("HOME", &self.env.home_root);
        cmd
    }
}

impl Drop for ZellijRoom {
    fn drop(&mut self) {
        let _ = self
            .zellij()
            .args(["delete-session", &self.session_name, "--force"])
            .bounded_output();
        let _ = tmux_output(&self.tmux_tmpdir, &["kill-server"]);
    }
}

fn live_zellij_session_name(line: &str) -> Option<String> {
    let clean = line.trim();
    let name = clean.split_whitespace().next()?;
    (!clean.contains("EXITED")).then(|| name.to_owned())
}

fn tmux_output(tmpdir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .scrub_session_env()
        .env("TMUX_TMPDIR", tmpdir)
        .args(args)
        .output()
        .expect("spawn tmux")
}
