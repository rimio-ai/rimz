//! Live regression tests for the one-root/one-backend room invariant.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::{CommandTimeoutExt, Env, ScrubSessionEnvExt};

#[test]
fn room_name_tracks_state_dir_without_rebirthing_a_live_old_name() {
    let Some(room) = TmuxRoom::start() else {
        return;
    };
    let paths = room.env.state_path_for(&room.env.project_root);
    let dir_name = paths.dir_name.as_str();
    assert_eq!(room.tmux_sessions(), vec![dir_name]);
    let mut record = rimz::workspace::record::read(&paths.workspace_record).unwrap();
    assert_eq!(record.session_name, dir_name);
    let owner = (
        record.rimz_bin.clone(),
        record.rimz_build.clone(),
        record.pins.clone(),
    );
    let old = "rimz-old-123456";
    assert!(
        tmux_output(
            &room.env.runtime_root,
            &["rename-session", "-t", dir_name, old]
        )
        .status
        .success()
    );
    record.session_name = old.to_owned();
    rimz::workspace::record::write(&paths, &record).unwrap();
    let session_id = tmux_output(
        &room.env.runtime_root,
        &["display-message", "-p", "-t", old, "#{session_id}"],
    )
    .stdout;

    let list = room
        .rimz()
        .args(["list", "--json"])
        .bounded_output()
        .unwrap();
    assert!(list.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(rows[0]["session_name"], old);
    assert_eq!(rows[0]["running_on"], "tmux");
    let agents = room
        .rimz()
        .args(["agents", "list"])
        .envs(rimz::workspace::pin_env(
            &record.workspace_id,
            &record.project_root,
        ))
        .bounded_output()
        .unwrap();
    assert!(
        agents.status.success(),
        "{}",
        String::from_utf8_lossy(&agents.stderr)
    );
    for args in [vec!["start"], vec!["attach", "--print"]] {
        let output = room.rimz().args(args).bounded_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(old));
        assert_eq!(room.tmux_sessions(), vec![old]);
        assert_eq!(
            tmux_output(
                &room.env.runtime_root,
                &["display-message", "-p", "-t", old, "#{session_id}"]
            )
            .stdout,
            session_id
        );
    }
    let preserved = rimz::workspace::record::read(&paths.workspace_record).unwrap();
    assert_eq!(
        (preserved.rimz_bin, preserved.rimz_build, preserved.pins),
        owner
    );
    let reset = room
        .rimz()
        .args(["--mux", "tmux", "reset", "--yes"])
        .bounded_output()
        .unwrap();
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert_eq!(room.tmux_sessions(), vec![dir_name]);
    assert_eq!(
        rimz::workspace::record::read(&paths.workspace_record)
            .unwrap()
            .session_name,
        dir_name
    );
    assert_eq!(
        String::from_utf8(
            tmux_output(
                &room.env.runtime_root,
                &["display-message", "-p", "-t", dir_name, "#S"]
            )
            .stdout
        )
        .unwrap()
        .trim(),
        dir_name
    );
    for args in [vec!["list"], vec!["gc", "--dry-run"]] {
        let output = room.rimz().args(args).bounded_output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(dir_name));
    }
}

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
        stdout.contains("tmux -S")
            && stdout.contains("attach")
            && stdout.contains(&room.session_name),
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
fn attach_purges_corrupt_zellij_resurrection_cache() {
    let Some(room) = ZellijRoom::start() else {
        return;
    };
    let cache_dir = room
        .env
        .home_root
        .join("zellij/contract_version_1/session_info")
        .join(&room.session_name);
    std::fs::create_dir_all(&cache_dir).expect("mkdir resurrection cache");
    std::fs::write(
        cache_dir.join("session-layout.kdl"),
        "layout { this is not kdl",
    )
    .expect("write corrupt resurrection layout");

    let output = room
        .rimz()
        .args(["attach", "--print"])
        .bounded_output()
        .expect("run printed attach");

    assert!(
        output.status.success(),
        "printed attach should succeed: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !cache_dir.exists(),
        "attach must remove the corrupt resurrection cache before handing off to zellij",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("zellij attach") && stdout.contains(&room.session_name),
        "attach should still print a usable zellij command, got: {stdout}",
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

#[test]
fn reset_explicit_rival_refuses_before_teardown() {
    let Some(room) = ZellijRoom::start() else {
        return;
    };

    let paths = room.env.state_path_for(&room.env.project_root);
    let archive_count_before = archive_entry_count(&paths.events_archive_dir);
    let output = room
        .rimz()
        .args(["--mux", "tmux", "reset", "--yes"])
        .bounded_output()
        .expect("run rival reset");

    assert!(
        !output.status.success(),
        "rival reset should fail: {:?}",
        output.status,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running under")
            && stderr.contains("zellij")
            && stderr.contains("tmux")
            && stderr.contains(&room.session_name),
        "stderr should describe the backend conflict, got: {stderr}",
    );
    assert!(
        room.zellij_sessions().contains(&room.session_name),
        "refused reset must leave the zellij room live",
    );
    assert!(
        !room.tmux_sessions().contains(&room.session_name),
        "refused reset must not birth a tmux room",
    );
    assert_eq!(
        archive_count_before,
        archive_entry_count(&paths.events_archive_dir),
        "refused reset must not archive room records",
    );
}

#[test]
fn reset_refuses_an_unbirthable_default_rebirth_before_teardown() {
    if which::which("zellij").is_err() {
        crate::common::skip("zellij not on PATH");
        return;
    }
    let Some(room) = TmuxRoom::start() else {
        return;
    };
    std::fs::write(
        room.env.rimz_home().join("config.toml"),
        "[mux]\ndefault = \"zellij\"\n",
    )
    .expect("write machine config");

    let paths = room.env.state_path_for(&room.env.project_root);
    let archive_count_before = archive_entry_count(&paths.events_archive_dir);
    let output = room
        .rimz()
        .args(["reset", "--yes"])
        .env("ZELLIJ_SOCKET_DIR", format!("/tmp/{}", "x".repeat(140)))
        .bounded_output()
        .expect("run reset");

    assert!(
        !output.status.success(),
        "reset should refuse the zellij rebirth"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("export ZELLIJ_SOCKET_DIR=/tmp/zellij"),
        "stderr should carry the socket fix, got: {stderr}",
    );
    assert!(
        room.tmux_sessions().contains(&room.session_name),
        "refused reset must leave the tmux room live",
    );
    assert_eq!(
        archive_count_before,
        archive_entry_count(&paths.events_archive_dir),
        "refused reset must not archive room records",
    );
}

/// A room started from an agent's tree, whose `TMPDIR` is the agent's temp
/// unit, gives its panes the `TMPDIR` the launch saved, or none, and drops
/// the provider temp roots that launch pointed at the unit.
#[test]
fn a_room_started_from_an_agent_gives_panes_the_user_tmpdir() {
    for saved in ["user-tmp", ""] {
        let scratch = tempfile::tempdir().expect("scratch");
        let unit = scratch.path().join("unit");
        let user = scratch.path().join(saved);
        std::fs::create_dir_all(&unit).expect("mkdir unit");
        std::fs::create_dir_all(&user).expect("mkdir user tmp");
        let saved = if saved.is_empty() {
            String::new()
        } else {
            user.display().to_string()
        };
        let extra = [
            ("TMPDIR", unit.to_str().expect("utf8 unit")),
            ("RIMZ_USER_TMPDIR", saved.as_str()),
            ("CLAUDE_CODE_TMPDIR", unit.to_str().expect("utf8 unit")),
            ("RIMZ_TEMP_ROOT_KEYS", "CLAUDE_CODE_TMPDIR"),
        ];
        let expected = if saved.is_empty() {
            "unset|unset|unset|unset".to_owned()
        } else {
            format!("{saved}|unset|unset|unset")
        };
        let script = |marker: &Path| {
            format!(
                "printf '%s|%s|%s|%s' \"${{TMPDIR-unset}}\" \"${{RIMZ_USER_TMPDIR-unset}}\" \"${{CLAUDE_CODE_TMPDIR-unset}}\" \"${{RIMZ_TEMP_ROOT_KEYS-unset}}\" > '{}'; sleep 60",
                marker.display()
            )
        };

        if let Some(room) = TmuxRoom::start_with(&extra) {
            let marker = scratch.path().join("tmux-pane");
            let output = tmux_output(
                &room.env.runtime_root,
                &[
                    "new-window",
                    "-d",
                    "-t",
                    &room.session_name,
                    &script(&marker),
                ],
            );
            assert!(output.status.success(), "tmux new-window failed");
            assert_eq!(wait_for_marker(&marker), expected, "tmux, saved {saved:?}");
        }
        if let Some(room) = ZellijRoom::start_with(&extra) {
            let marker = scratch.path().join("zellij-pane");
            let status = room
                .zellij()
                .args(["--session", &room.session_name, "run", "--"])
                .args(["sh", "-c", &script(&marker)])
                .bounded_status()
                .expect("zellij run");
            assert!(status.success(), "zellij run failed");
            assert_eq!(
                wait_for_marker(&marker),
                expected,
                "zellij, saved {saved:?}"
            );
        }
    }
}

#[test]
fn a_nested_room_gives_panes_only_its_own_mux_context() {
    let scratch = tempfile::tempdir().expect("scratch");
    let mut failures = Vec::new();
    let outer_zellij = [
        ("ZELLIJ", "0"),
        ("ZELLIJ_SESSION_NAME", "outer"),
        ("ZELLIJ_PANE_ID", "terminal_1"),
    ];
    if let Some(room) = TmuxRoom::start_with(&outer_zellij) {
        let marker = scratch.path().join("tmux-env");
        let doctor = scratch.path().join("tmux-doctor");
        let socket = rimz::mux::tmux::managed_server_socket_path_under(&room.env.runtime_root);
        let output = Command::new("tmux")
            .scrub_session_env()
            .envs(outer_zellij)
            .arg("-S")
            .arg(socket)
            .args(["new-window", "-d", "-t", &room.session_name])
            .arg(pane_mux_probe(&room.env, &marker, &doctor))
            .bounded_output()
            .expect("tmux new-window");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        check_pane_mux("tmux", &marker, &doctor, &mut failures);
    }
    let outer_tmux = [("TMUX", "/nonexistent,1,0"), ("TMUX_PANE", "%0")];
    if let Some(room) = ZellijRoom::start_with(&outer_tmux) {
        let marker = scratch.path().join("zellij-env");
        let doctor = scratch.path().join("zellij-doctor");
        let output = room
            .zellij()
            .envs(outer_tmux)
            .args(["--session", &room.session_name, "run", "--", "sh", "-c"])
            .arg(pane_mux_probe(&room.env, &marker, &doctor))
            .bounded_output()
            .expect("zellij run");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        check_pane_mux("zellij", &marker, &doctor, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_room_repairs_outer_mux_context_on_an_existing_tmux_server() {
    use rimz::mux::MuxBackend;

    let Some(room) = TmuxRoom::new() else {
        return;
    };
    let socket = rimz::mux::tmux::managed_server_socket_path_under(&room.env.runtime_root);
    std::fs::create_dir_all(socket.parent().expect("socket parent")).expect("mkdir socket dir");
    let output = Command::new("tmux")
        .scrub_session_env()
        .env("HOME", &room.env.home_root)
        .env("XDG_RUNTIME_DIR", &room.env.runtime_root)
        .env("SHELL", "/bin/sh")
        .envs([
            ("ZELLIJ", "0"),
            ("ZELLIJ_PANE_ID", "terminal_1"),
            ("ZELLIJ_SESSION_NAME", "outer"),
        ])
        .arg("-S")
        .arg(&socket)
        .args(["new-session", "-d", "-s", "pre-fix", "sleep 60"])
        .bounded_output()
        .expect("birth contaminated tmux server");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Inspect the original birth shell before room start can repurpose it as sidebar.
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    let command = room.rimz();
    rimz::mux::TmuxBackend::with_socket(&socket)
        .ensure_session(&rimz::mux::SessionOptions {
            session_name: room.session_name.clone(),
            workspace_id: workspace.workspace_id,
            project_root: room.env.project_root.clone(),
            extra_env: command
                .get_envs()
                .filter_map(|(key, value)| {
                    value.map(|value| {
                        (
                            key.to_string_lossy().into_owned(),
                            value.to_string_lossy().into_owned(),
                        )
                    })
                })
                .collect(),
            cwd: room.env.project_root.clone(),
            config: Default::default(),
            detected_size: None,
            truecolor: false,
        })
        .expect("ensure room on contaminated server");
    let first = tmux_output(
        &room.env.runtime_root,
        &[
            "display-message",
            "-p",
            "-t",
            &room.session_name,
            "#{pane_id} #{pane_pid}",
        ],
    );
    assert!(first.status.success());
    let first = String::from_utf8(first.stdout).unwrap();
    let (pane, pid) = first.trim().split_once(' ').expect("first pane identity");
    let marker = room.env.project_root.join("first-env");
    let doctor = room.env.project_root.join("first-doctor");
    assert!(
        tmux_output(
            &room.env.runtime_root,
            &[
                "send-keys",
                "-t",
                pane,
                "-l",
                &pane_mux_probe(&room.env, &marker, &doctor)
            ]
        )
        .status
        .success()
    );
    assert!(
        tmux_output(&room.env.runtime_root, &["send-keys", "-t", pane, "Enter"])
            .status
            .success()
    );
    let mut failures = Vec::new();
    check_pane_mux("tmux", &marker, &doctor, &mut failures);
    #[cfg(target_os = "linux")]
    {
        let pid = pid.parse().expect("first pane pid");
        for key in ["ZELLIJ", "ZELLIJ_PANE_ID", "ZELLIJ_SESSION_NAME"] {
            if let Some(value) = rimz::proc::env_var(pid, key) {
                failures.push(format!("first-window process inherited {key}={value}"));
            }
        }
        assert!(
            rimz::proc::env_var(pid, "TMUX").is_some(),
            "first pane's own tmux identity"
        );
        assert_eq!(rimz::proc::env_var(pid, "TMUX_PANE").as_deref(), Some(pane));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;

    room.open(&[]);
    let marker = room.env.project_root.join("later-env");
    let doctor = room.env.project_root.join("later-doctor");
    assert!(
        tmux_output(
            &room.env.runtime_root,
            &[
                "new-window",
                "-d",
                "-t",
                &room.session_name,
                &pane_mux_probe(&room.env, &marker, &doctor)
            ]
        )
        .status
        .success()
    );
    check_pane_mux("tmux", &marker, &doctor, &mut failures);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn pane_mux_probe(env: &Env, marker: &Path, doctor: &Path) -> String {
    let command = env.rimz();
    let pinned_env = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                shlex::try_quote(&format!(
                    "{}={}",
                    key.to_string_lossy(),
                    value.to_string_lossy()
                ))
                .expect("shell quote fixture env")
                .into_owned()
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "for key in TMUX TMUX_PANE ZELLIJ ZELLIJ_PANE_ID ZELLIJ_SESSION_NAME; do if value=$(printenv \"$key\"); then printf '%s=%s\\n' \"$key\" \"$value\"; fi; done > {marker}; env {pinned_env} {rimz} doctor --json --no-log-text > {doctor}; printf '|done' >> {marker}; sleep 60",
        marker = shlex::try_quote(&marker.to_string_lossy()).expect("shell quote marker"),
        doctor = shlex::try_quote(&doctor.to_string_lossy()).expect("shell quote doctor marker"),
        rimz =
            shlex::try_quote(&command.get_program().to_string_lossy()).expect("shell quote rimz"),
    )
}

fn check_pane_mux(mux: &str, marker: &Path, doctor: &Path, failures: &mut Vec<String>) {
    let seen = wait_for_marker(marker);
    let names = seen
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
    let expected: &[&str] = match mux {
        "tmux" => &["TMUX", "TMUX_PANE"],
        "zellij" => &["ZELLIJ", "ZELLIJ_PANE_ID", "ZELLIJ_SESSION_NAME"],
        _ => unreachable!("the test drives only tmux and Zellij"),
    };
    if names != expected {
        failures.push(format!(
            "{mux} pane environment: expected {expected:?}, got {seen:?}"
        ));
    }
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(doctor).expect("doctor marker"))
            .expect("doctor JSON");
    let selected = &report["mux"]["ready"]["name"];
    if selected != mux {
        failures.push(format!(
            "{mux} pane resolved backend: expected {mux}, got {selected}"
        ));
    }
}

fn wait_for_marker(marker: &Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(seen) = std::fs::read_to_string(marker)
            && seen.contains('|')
        {
            return seen;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pane never wrote {}",
            marker.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

struct TmuxRoom {
    env: Env,
    session_name: String,
    tmux_tmpdir: PathBuf,
}

impl TmuxRoom {
    fn start() -> Option<Self> {
        Self::start_with(&[])
    }

    /// Start the room from a process carrying `extra` over the fixture env.
    fn start_with(extra: &[(&str, &str)]) -> Option<Self> {
        let room = Self::new()?;
        room.open(extra);
        Some(room)
    }

    fn new() -> Option<Self> {
        if which::which("tmux").is_err() {
            crate::common::skip("tmux not on PATH");
            return None;
        }
        let env = Env::new();
        let tmux_tmpdir = env.project_root.join("tmux");
        std::fs::create_dir_all(&tmux_tmpdir).expect("mkdir tmux tmpdir");
        let workspace = env.resolve_workspace(&env.project_root);
        let session_name = workspace.session_name;

        Some(Self {
            env,
            session_name,
            tmux_tmpdir,
        })
    }

    fn open(&self, extra: &[(&str, &str)]) {
        let output = self
            .rimz()
            .args(["--mux", "tmux", "start"])
            .envs(extra.iter().copied())
            .bounded_output()
            .expect("run tmux start");
        assert!(
            output.status.success(),
            "tmux start failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn rimz(&self) -> Command {
        let mut cmd = self.env.rimz();
        cmd.env("TMUX_TMPDIR", &self.tmux_tmpdir);
        cmd
    }

    fn tmux_sessions(&self) -> Vec<String> {
        let output = tmux_output(
            &self.env.runtime_root,
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
        let _ = tmux_output(&self.env.runtime_root, &["kill-server"]);
    }
}

struct ZellijRoom {
    env: Env,
    session_name: String,
    tmux_tmpdir: PathBuf,
}

impl ZellijRoom {
    fn start() -> Option<Self> {
        Self::start_with(&[])
    }

    /// Start the room from a process carrying `extra` over the fixture env.
    fn start_with(extra: &[(&str, &str)]) -> Option<Self> {
        if which::which("zellij").is_err() {
            crate::common::skip("zellij not on PATH");
            return None;
        }
        if which::which("tmux").is_err() {
            crate::common::skip("tmux not on PATH");
            return None;
        }
        let env = Env::new();
        let tmux_tmpdir = env.project_root.join("tmux");
        std::fs::create_dir_all(&tmux_tmpdir).expect("mkdir tmux tmpdir");
        let workspace = env.resolve_workspace(&env.project_root);
        let session_name = workspace.session_name;

        let output = {
            let mut cmd = env.rimz();
            pin_zellij_shared_env(&env, &mut cmd);
            cmd.args(["--mux", "zellij", "start"])
                .env("TMUX_TMPDIR", &tmux_tmpdir)
                .envs(extra.iter().copied())
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
        pin_zellij_shared_env(&self.env, &mut cmd);
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
            &self.env.runtime_root,
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
            .env("RIMZ_HOME", self.env.rimz_home())
            .env("XDG_STATE_HOME", self.env.state_root())
            .env("XDG_CONFIG_HOME", self.env.config_root())
            .env("XDG_CACHE_HOME", &self.env.home_root)
            .env("HOME", &self.env.home_root)
            .env("TMPDIR", &self.env.home_root);
        cmd
    }
}

fn pin_zellij_shared_env(env: &Env, cmd: &mut Command) {
    cmd.env("XDG_CACHE_HOME", &env.home_root)
        .env("TMPDIR", &env.home_root);
}

impl Drop for ZellijRoom {
    fn drop(&mut self) {
        let _ = self
            .zellij()
            .args(["delete-session", &self.session_name, "--force"])
            .bounded_output();
        let socket = rimz::mux::tmux::managed_server_socket_path_under(&self.env.runtime_root);
        let _ = Command::new("tmux")
            .scrub_session_env()
            .arg("-S")
            .arg(socket)
            .arg("kill-server")
            .bounded_output();
    }
}

fn live_zellij_session_name(line: &str) -> Option<String> {
    let clean = line.trim();
    let name = clean.split_whitespace().next()?;
    (!clean.contains("EXITED")).then(|| name.to_owned())
}

fn archive_entry_count(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.count(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => panic!("read archive dir {}: {err}", dir.display()),
    }
}

/// Query the same server `rimz` uses: the managed endpoint derived from the
/// room's runtime root. `TMUX_TMPDIR` no longer decides where RimZ's sessions
/// live, so isolating on it alone would inspect an empty default server.
fn tmux_output(runtime_root: &Path, args: &[&str]) -> std::process::Output {
    let socket = rimz::mux::tmux::managed_server_socket_path_under(runtime_root);
    Command::new("tmux")
        .scrub_session_env()
        .arg("-S")
        .arg(&socket)
        .args(args)
        .bounded_output()
        .expect("spawn tmux")
}
