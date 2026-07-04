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

fn tmux_output(tmpdir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .scrub_session_env()
        .env("TMUX_TMPDIR", tmpdir)
        .args(args)
        .output()
        .expect("spawn tmux")
}
