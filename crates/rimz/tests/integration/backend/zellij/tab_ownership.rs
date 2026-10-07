use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::{AgentKind, MuxName, PaneId};
use rimz::mux::tab_name::TabOwnerRecord;

use super::support::ListedPane;
use crate::common::{CommandTimeoutExt, Env};

struct TabNamingRoom {
    env: Env,
    session: String,
    agent_bin: PathBuf,
    ready: PathBuf,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    client: Box<dyn portable_pty::Child + Send + Sync>,
}

impl TabNamingRoom {
    fn new() -> Self {
        let env = Env::new();
        env.install_agent_hooks("claude");
        std::fs::create_dir_all(env.rimz_home()).unwrap();
        std::fs::write(
            env.rimz_home().join("config.toml"),
            "[agents]\nisolation = \"host\"\n",
        )
        .unwrap();
        for profile in ["opus", "brainstormer"] {
            crate::common::write_definition(
                &env,
                "agents",
                profile,
                "description: Naming fixture\nagent: claude\ntools: []",
                "",
            );
        }
        let agent_bin = env.home_root.join("agent-bin");
        std::fs::create_dir(&agent_bin).unwrap();
        let shim = agent_bin.join("claude");
        std::fs::write(&shim, "#!/bin/bash\nset -e\n\
            case \"${1:-}\" in\n\
              --version) printf 'claude 0.0.0\\n'; exit 0;;\n\
              auth|login) exit 1;;\n\
            esac\n\
            printf '{\"hook_event_name\":\"SessionStart\",\"session_id\":\"tab-name-%s\"}\\n' \"$$\" | \
            RIMZ_AGENT_PID=$$ \"$RIMZ_TEST_RIMZ_BIN\" hooks feed --source claude >/dev/null\n\
            printf '%s' \"$$\" > \"$RIMZ_TEST_AGENT_READY/$ZELLIJ_PANE_ID\"\n\
            exec -a claude sleep 300\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", agent_bin.display(), std::env::var("PATH").unwrap());
        env.write_config(
            &env.project_root,
            &toml::to_string(&serde_json::json!({
                "agents": [{"name": "claude", "env": {"PATH": path}}],
            }))
            .unwrap(),
        );
        env.rimz()
            .args(["trust", "grant"])
            .assert_success_within_timeout("trust fixture provider PATH");
        let ready = env.home_root.join("tab-name-ready");
        std::fs::create_dir(&ready).unwrap();
        let session = env.resolve_workspace(&env.project_root).session_name;
        env.rimz()
            .env("PATH", &path)
            .env("RIMZ_TEST_AGENT_READY", &ready)
            .env("RIMZ_TEST_RIMZ_BIN", env.rimz_bin())
            .args(["--mux", "zellij", "start", "--no-attach"])
            .assert_success_within_timeout("start real Zellij producer");
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("zellij");
        env.pin_pty_command(&mut command);
        command.env("TERM", "xterm-256color");
        command.args(["attach", &session]);
        let mut client = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().unwrap();
        std::thread::spawn(move || {
            let _ = io::copy(&mut reader, &mut io::sink());
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let output = env
                .rimz_at(Path::new("zellij"))
                .args(["--session", &session, "action", "list-clients"])
                .bounded_output()
                .unwrap();
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("terminal_")
            {
                break;
            }
            assert!(
                client.try_wait().unwrap().is_none(),
                "Zellij client exited before registration"
            );
            assert!(Instant::now() < deadline, "Zellij client did not register");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            env,
            session,
            agent_bin,
            ready,
            _master: pty.master,
            client,
        }
    }

    fn native(&self) -> Vec<ListedPane> {
        super::support::expect_list_panes(&self.env.runtime_root, &self.session).panes
    }

    fn action(&self, args: &[&str]) {
        self.env
            .rimz_at(Path::new("zellij"))
            .args(["--session", &self.session, "action"])
            .args(args)
            .assert_success_within_timeout("Zellij naming action");
    }

    fn owners(&self) -> BTreeMap<u64, TabOwnerRecord> {
        std::fs::read(self.env.runtime_paths().lane_path("tab-owners.json"))
            .ok()
            .map(|bytes| {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                serde_json::from_value(value["tabs"].clone()).unwrap()
            })
            .unwrap_or_default()
    }

    fn launch(&self, profile: &str, target: Option<&PaneId>) -> (PaneId, String) {
        let existing = std::fs::read_dir(&self.ready)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        let mut command = self.env.rimz();
        command
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.agent_bin.display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("RIMZ_TEST_AGENT_READY", &self.ready)
            .env("RIMZ_TEST_RIMZ_BIN", self.env.rimz_bin())
            .args(["--mux", "zellij", "agents", profile]);
        if let Some(target) = target {
            command
                .env("ZELLIJ", "0")
                .env("ZELLIJ_SESSION_NAME", &self.session)
                .env(
                    "ZELLIJ_PANE_ID",
                    target.raw().strip_prefix("terminal_").unwrap(),
                )
                .args(["--bg", "--detach", "check tab naming"]);
        } else {
            command.arg("--new-tab");
        }
        command.assert_success_within_timeout("launch naming agent");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for entry in std::fs::read_dir(&self.ready).unwrap().map(Result::unwrap) {
                if existing.contains(&entry.file_name()) {
                    continue;
                }
                let pid = std::fs::read_to_string(entry.path()).unwrap();
                if !pid.is_empty() {
                    return (
                        PaneId::from_parts(
                            MuxName::Zellij,
                            format!("terminal_{}", entry.file_name().to_string_lossy()),
                        ),
                        pid,
                    );
                }
            }
            assert!(Instant::now() < deadline, "agent did not start");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_name(&self, anchor: &PaneId, expected: &str, after_ms: u64) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let frame = rimz::sidebar::cache::read_snapshot_cache(
                &self.env.runtime_paths().pane_frame_path(),
                &self.session,
            );
            let actual = self
                .native()
                .into_iter()
                .find(|pane| !pane.is_plugin && format!("terminal_{}", pane.id) == anchor.raw())
                .and_then(|pane| pane.tab_name);
            if let Some(frame) = frame.as_ref()
                && frame.produced_at_ms > after_ms
                && frame.tabs.iter().any(|tab| {
                    tab.name.as_deref() == Some(expected)
                        && tab.panes.iter().any(|pane| pane.pane_id == *anchor)
                })
                && actual.as_deref() == Some(expected)
            {
                return frame.produced_at_ms;
            }
            assert!(
                Instant::now() < deadline,
                "producer did not observe {expected:?}: {frame:?}; actual: {actual:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn end(&self, pid: &str) -> u64 {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid.parse().unwrap()),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let key = (
            AgentKind::new_unchecked("claude"),
            format!("tab-name-{pid}").into(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self
            .env
            .store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .ended
            .contains(&key)
        {
            assert!(Instant::now() < deadline, "agent exit was not recorded");
            std::thread::sleep(Duration::from_millis(25));
        }
        rimz::utils::time::unix_now_ms()
    }
}

impl Drop for TabNamingRoom {
    fn drop(&mut self) {
        let _ = self.client.kill();
        let _ = self.client.wait();
    }
}

#[test]
fn producer_leaves_an_unowned_tab_alone() {
    require_zellij!();
    let room = TabNamingRoom::new();
    room.action(&["new-tab", "--name", "btop", "--", "sleep", "300"]);
    let pane = super::support::poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(room.native()),
        |panes| {
            panes
                .iter()
                .any(|pane| !pane.is_plugin && pane.tab_name.as_deref() == Some("btop"))
        },
        "unowned tab",
    )
    .into_iter()
    .find(|pane| !pane.is_plugin && pane.tab_name.as_deref() == Some("btop"))
    .unwrap();
    let anchor = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id));
    let first = room.wait_name(&anchor, "btop", 0);
    room.wait_name(&anchor, "btop", first);
    assert!(!room.owners().contains_key(&pane.tab_id));
    let (agent, pid) = room.launch("brainstormer", Some(&anchor));
    let first = room.wait_name(&agent, "btop", first);
    room.wait_name(&agent, "btop", first);
    assert!(!room.owners().contains_key(&pane.tab_id));
    let ended = room.end(&pid);
    room.wait_name(&anchor, "btop", ended);
    assert!(!room.owners().contains_key(&pane.tab_id));
}

#[test]
fn producer_holds_the_founder_name_then_follows_the_peer() {
    require_zellij!();
    let room = TabNamingRoom::new();
    let (founder, founder_pid) = room.launch("opus", None);
    let (peer, peer_pid) = room.launch("brainstormer", Some(&founder));
    let first = room.wait_name(&peer, "opus", 0);
    room.wait_name(&peer, "opus", first);
    let panes = room.native();
    let tab = panes
        .iter()
        .find(|pane| !pane.is_plugin && format!("terminal_{}", pane.id) == founder.raw())
        .unwrap()
        .tab_id;
    assert!(panes.iter().any(|pane| !pane.is_plugin
        && format!("terminal_{}", pane.id) == peer.raw()
        && pane.tab_id == tab));
    assert_eq!(
        room.owners().get(&tab).map(|owner| &owner.founders),
        Some(&vec![founder.clone()])
    );
    let ended = room.end(&founder_pid);
    room.wait_name(&peer, "brainstormer", ended);
    assert_eq!(room.owners()[&tab].base, "brainstormer");
    let ended = room.end(&peer_pid);
    room.wait_name(&founder, "sh", ended);
    assert!(!room.owners().contains_key(&tab));
    assert_eq!(
        room.native()
            .iter()
            .find(|pane| !pane.is_plugin && format!("terminal_{}", pane.id) == founder.raw())
            .unwrap()
            .title
            .as_deref(),
        Some("opus")
    );
}

#[test]
fn producer_keeps_a_user_name_over_an_agent() {
    require_zellij!();
    let room = TabNamingRoom::new();
    let (founder, founder_pid) = room.launch("opus", None);
    let tab = room
        .native()
        .iter()
        .find(|pane| !pane.is_plugin && format!("terminal_{}", pane.id) == founder.raw())
        .unwrap()
        .tab_id;
    room.action(&["rename-tab-by-id", &tab.to_string(), "my tab"]);
    let (peer, peer_pid) = room.launch("brainstormer", Some(&founder));
    let first = room.wait_name(&peer, "my tab", 0);
    room.wait_name(&peer, "my tab", first);
    let ended = room.end(&founder_pid);
    room.wait_name(&peer, "my tab", ended);
    let ended = room.end(&peer_pid);
    room.wait_name(&founder, "my tab", ended);
}
