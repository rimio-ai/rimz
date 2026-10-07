//! Live room-host process shape, recovery, and attachment eviction on Linux.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::process::Stdio;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use sha2::Digest;

use super::*;

const SESSION: &str = "room-host";
const POLL: Duration = Duration::from_millis(100);
/// Budget for an assertion on a state that already holds: it only outlasts a
/// forked child that still reads as its parent in the census.
const CENSUS_RETRY: Duration = Duration::from_secs(2);
const OVERRIDES: &[(&str, &str)] = &[
    ("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "8000"),
    ("RIMZ_TEST_SIDEBAR_HOST_PROBE_INTERVAL_MS", "250"),
];
const HOST_KILL_STABLE: Duration = Duration::from_secs(20);
const HOST_KILL_MARGIN: Duration = Duration::from_secs(8);
const HOST_KILL_OVERRIDES: &[(&str, &str)] = &[
    ("RIMZ_TEST_SIDEBAR_STABLE_RUN_MS", "20000"),
    ("RIMZ_TEST_SIDEBAR_HOST_PROBE_INTERVAL_MS", "250"),
    ("RIMZ_TEST_SIDEBAR_HANDOFF_GRACE_MS", "2000"),
    // Self-close probes are outside this scenario; stressed copies can outlast a minute.
    ("RIMZ_TEST_SIDEBAR_PANE_PROBE_INTERVAL_MS", "600000"),
];
const TAB_MARKERS: [&str; 2] = ["host-tab-one", "host-tab-two"];

#[derive(Clone, Debug)]
struct Proc {
    pid: u32,
    ppid: u32,
    start_token: String,
    pane: Option<PaneId>,
    argv: Vec<std::ffi::OsString>,
}

impl Proc {
    fn live(&self) -> bool {
        rimz::proc::process_is_live(self.pid, Some(&self.start_token))
    }

    fn same_process(&self, other: &Self) -> bool {
        self.pid == other.pid && self.start_token == other.start_token
    }
}

#[derive(Debug, Default)]
struct Census {
    hosts: Vec<Proc>,
    supervisors: Vec<Proc>,
    workers: Vec<Proc>,
}

struct HostGuard {
    image: (u64, u64),
    workspace: WorkspaceId,
    warm_host: Option<std::process::Child>,
}

impl HostGuard {
    fn census(&self) -> Census {
        let mut census = Census::default();
        for info in rimz::proc::list_processes() {
            if !info.cmdline.contains(self.workspace.as_str()) {
                continue;
            }
            let Ok(image) = std::fs::metadata(format!("/proc/{}/exe", info.pid)) else {
                continue;
            };
            if (image.dev(), image.ino()) != self.image {
                continue;
            }
            let Some(argv) = rimz::proc::argv(info.pid) else {
                continue;
            };
            let has_pair = |flag: &str, value: &str| {
                argv.windows(2)
                    .any(|pair| pair[0] == flag && pair[1] == value)
            };
            if !has_pair("--workspace-id", self.workspace.as_str())
                || !has_pair("--session-name", SESSION)
            {
                continue;
            }
            let Some(start_token) = rimz::proc::process_start_token(info.pid) else {
                continue;
            };
            let pane = rimz::proc::env_var(info.pid, "TMUX_PANE")
                .map(|raw| PaneId::from_parts(MuxName::Tmux, raw))
                .or_else(|| {
                    rimz::proc::env_var(info.pid, "ZELLIJ_PANE_ID")
                        .map(|raw| PaneId::from_parts(MuxName::Zellij, format!("terminal_{raw}")))
                });
            let process = Proc {
                pid: info.pid,
                ppid: info.ppid,
                start_token,
                pane,
                argv,
            };
            if !process.live() {
                continue;
            }
            let role = process
                .argv
                .windows(2)
                .find_map(|pair| (pair[0] == "sidebar").then(|| pair[1].to_str()).flatten());
            match role {
                Some("host") => census.hosts.push(process),
                Some("serve") if rimz::proc::env_var(info.pid, "RIMZ_SIDEBAR_WORKER").is_some() => {
                    census.workers.push(process);
                }
                Some("serve") => census.supervisors.push(process),
                _ => {}
            }
        }
        census
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        for host in self.census().hosts {
            if host.live() {
                let _ = kill(Pid::from_raw(host.pid as i32), Signal::SIGKILL);
            }
        }
        if let Some(child) = &mut self.warm_host {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

type Attachments = HashMap<PaneId, SidebarInstanceId>;

#[derive(Debug)]
struct Snapshot {
    census: Census,
    attachments: Attachments,
    beats: HashMap<PaneId, jiff::Timestamp>,
}

impl Snapshot {
    fn settled(&self, panes: &[PaneId; 2]) -> bool {
        self.census.hosts.len() == 1
            && self.census.workers.is_empty()
            && self.census.supervisors.len() == 2
            && self.attachments.len() == 2
            && panes.iter().all(|pane| {
                self.attachments.contains_key(pane)
                    && self
                        .census
                        .supervisors
                        .iter()
                        .filter(|process| process.pane.as_ref() == Some(pane))
                        .count()
                        == 1
            })
    }
}

enum LiveRoom {
    Tmux {
        socket: PathBuf,
        _client: AttachProcess,
        _server: TmuxServerGuard,
        _runtime: TempDir,
    },
    Zellij {
        client: AttachedZellijScreen,
        cleanup: ZellijSessionGuard,
    },
}

struct Room {
    live: LiveRoom,
    hosts: HostGuard,
    env: Env,
    runtime: rimz::RuntimePaths,
    panes: [PaneId; 2],
    t0: Instant,
    released_at: Duration,
    steps: Vec<(Duration, String)>,
    seen_hosts: HashSet<(u32, String)>,
    seen_attachments: HashSet<(PaneId, SidebarInstanceId)>,
}

impl Room {
    fn new(mux: MuxName, fresh_start: bool) -> Option<Self> {
        if which::which(mux.as_str()).is_err() {
            crate::common::skip(match mux {
                MuxName::Tmux => "tmux not on PATH",
                MuxName::Zellij => "zellij not on PATH",
            });
            return None;
        }
        let rimz = rimz_bin()?;
        let env = Env::new();
        if env.skip_if_sandboxed() {
            return None;
        }
        env.record(&env.project_root);
        // `renderers` exists only in a testkit build, the one that honours the timing overrides.
        let probe = env
            .rimz()
            .args(["sidebar", "renderers", "--json"])
            .bounded_output()
            .expect("probe the testkit-only verb");
        assert!(
            probe.status.success(),
            "{} lacks the testkit feature the timing overrides need: {}",
            rimz.display(),
            String::from_utf8_lossy(&probe.stderr)
        );
        env.install_agent_hooks("codex");
        let fake_codex = fake_codex_bin(&env.home_root);
        let image = std::fs::metadata(&rimz).expect("fixture binary inode");
        let hosts = HostGuard {
            image: (image.dev(), image.ino()),
            workspace: env.workspace_id.clone(),
            warm_host: None,
        };
        let state = env.state_path_for(&env.project_root);
        let t0 = Instant::now();
        let gate = env.home_root.join("start-sidebars");
        let serve_line = |runtime: &Path| {
            let serve = sidebar_serve_line(
                &env,
                &rimz,
                runtime,
                mux.as_str(),
                SESSION,
                if fresh_start {
                    HOST_KILL_OVERRIDES
                } else {
                    OVERRIDES
                },
            );
            if fresh_start {
                format!(
                    "while [ ! -e {} ]; do sleep 0.05; done; {serve}",
                    shell_quote(&gate.display().to_string())
                )
            } else {
                serve
            }
        };
        let (live, runtime) = match mux {
            MuxName::Tmux => {
                let runtime = tempfile::Builder::new()
                    .prefix("rz")
                    .rand_bytes(6)
                    .tempdir()
                    .expect("short runtime dir");
                let paths = rimz::RuntimePaths::for_state_under(&state, runtime.path());
                let socket = managed_socket(runtime.path());
                let server = TmuxServerGuard::new(socket.clone());
                let cwd = env.project_root.display().to_string();
                for (index, marker) in TAB_MARKERS.iter().enumerate() {
                    let target = format!("{SESSION}:{index}");
                    let filler = format!("printf '{marker}\\n'; exec {} 600", fake_codex.display());
                    if index == 0 {
                        let output = Command::new("tmux")
                            .scrub_session_env()
                            .env("HOME", &env.home_root)
                            .env("XDG_RUNTIME_DIR", runtime.path())
                            .arg("-S")
                            .arg(&socket)
                            .args([
                                "new-session",
                                "-d",
                                "-s",
                                SESSION,
                                "-x",
                                "180",
                                "-y",
                                "40",
                                "-c",
                                &cwd,
                                &filler,
                            ])
                            .bounded_output()
                            .expect("birth marker-owned tmux server");
                        assert!(
                            output.status.success(),
                            "tmux birth failed: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    } else {
                        tmux(
                            &socket,
                            &["new-window", "-d", "-t", &target, "-c", &cwd, &filler],
                        );
                    }
                    let serve = serve_line(runtime.path());
                    tmux(
                        &socket,
                        &["split-window", "-h", "-l", "60", "-t", &target, &serve],
                    );
                }
                let parser = Arc::new(Mutex::new(vt100::Parser::new(40, 180, 0)));
                let mut command = CommandBuilder::new("tmux");
                command.scrub_session_env();
                command.arg("-S");
                command.arg(&socket);
                command.args(["attach-session", "-t", SESSION]);
                command.env("TERM", "xterm-256color");
                command.env("HOME", &env.home_root);
                command.env("RIMZ_HOME", env.rimz_home());
                command.env("XDG_RUNTIME_DIR", runtime.path());
                let client = AttachProcess::on_pty(command, &parser);
                (
                    LiveRoom::Tmux {
                        socket,
                        _client: client,
                        _server: server,
                        _runtime: runtime,
                    },
                    paths,
                )
            }
            MuxName::Zellij => {
                let cleanup = ZellijSessionGuard {
                    name: SESSION.to_owned(),
                    namespace: ZellijNamespace::new(),
                };
                let runtime = cleanup.namespace.path();
                let paths = rimz::RuntimePaths::for_state_under(&state, runtime);
                let serve = serve_line(runtime);
                let tabs = TAB_MARKERS
                    .iter()
                    .map(|marker| {
                        format!(
                            r#"tab name="{marker}" {{
    pane split_direction="vertical" {{
        pane size="33%" name="rimz-sidebar" {{ command "sh"; args "-c" {serve}; }}
        pane focus=true {{ command "sh"; args "-c" {filler}; }}
    }}
}}"#,
                            serve = serde_json::to_string(&serve).expect("KDL serve string"),
                            filler = serde_json::to_string(&format!(
                                "printf '{marker}\\n'; exec {} 600",
                                fake_codex.display()
                            ))
                            .expect("KDL filler string"),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let layout = env.home_root.join("room-host.kdl");
                std::fs::write(&layout, format!("layout {{\n{tabs}\n}}\n")).expect("write layout");
                let status = cleanup
                    .namespace
                    .command()
                    .args(["attach", "--create-background", SESSION, "options"])
                    .arg("--default-cwd")
                    .arg(&env.project_root)
                    .arg("--default-layout")
                    .arg(layout)
                    .bounded_status()
                    .expect("create two-tab session");
                assert!(status.success(), "create-background failed: {status}");
                let client = AttachedZellijScreen::new(&cleanup.namespace, SESSION, 180, 40);
                (LiveRoom::Zellij { client, cleanup }, paths)
            }
        };
        let mut room = Self {
            hosts,
            live,
            env,
            runtime,
            panes: std::array::from_fn(|_| PaneId::from_parts(mux, "pending")),
            t0,
            released_at: Duration::ZERO,
            steps: vec![(t0.elapsed(), "client attached".to_owned())],
            seen_hosts: HashSet::new(),
            seen_attachments: HashSet::new(),
        };
        if fresh_start {
            // Start the host before either supervisor can attach. The gate release
            // bounds both supervisors' later `started` clocks, even under load.
            let runtime = match &room.live {
                LiveRoom::Tmux { _runtime, .. } => _runtime.path(),
                LiveRoom::Zellij { cleanup, .. } => cleanup.namespace.path(),
            };
            let mut command = room.env.rimz();
            command
                .env("XDG_RUNTIME_DIR", runtime)
                .args([
                    "sidebar",
                    "host",
                    "--mux",
                    mux.as_str(),
                    "--workspace-id",
                    room.env.workspace_id.as_str(),
                    "--session-name",
                    SESSION,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            room.hosts.warm_host = Some(command.spawn().expect("prewarm room host"));
            let key = sha2::Sha256::digest(format!("{mux}\0{SESSION}"));
            let socket = room
                .runtime
                .sock_dir
                .join(format!("host.{}.sock", hex::encode(&key[..6])));
            room.wait(
                "host accepting connections before supervisor birth",
                CAPTURE_BUDGET,
                |snapshot| {
                    snapshot.census.hosts.len() == 1
                        && std::os::unix::net::UnixStream::connect(&socket).is_ok()
                },
            );
            room.released_at = room.step("supervisor startup gate released");
            std::fs::write(&gate, b"").expect("release supervisor startup");
        }
        let ready = room.wait(
            "two supervisor panes materialized",
            CAPTURE_BUDGET,
            |snapshot| {
                // A supervisor's child reads as a second supervisor until it execs.
                matches!(
                    snapshot.census.supervisors.as_slice(),
                    [a, b] if a.pane.is_some() && b.pane.is_some() && a.pane != b.pane
                )
            },
        );
        let mut panes: Vec<_> = ready
            .census
            .supervisors
            .iter()
            .map(|p| p.pane.clone().unwrap())
            .collect();
        panes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        room.panes = panes.try_into().expect("two supervisor panes");
        if let LiveRoom::Zellij { cleanup, .. } = &room.live {
            write_zellij_topology(&cleanup.namespace, &room.runtime, SESSION, false);
        }
        Some(room)
    }

    fn step(&mut self, name: impl Into<String>) -> Duration {
        let at = self.t0.elapsed();
        self.steps.push((at, name.into()));
        at
    }

    fn snapshot(&self) -> Snapshot {
        let heartbeats: Vec<_> = rimz::sidebar::live_sidebars(&self.runtime)
            .into_iter()
            .map(|sidebar| sidebar.heartbeat)
            .filter(|heartbeat| heartbeat.size.is_some() && heartbeat.session_name == SESSION)
            .collect();
        Snapshot {
            census: self.hosts.census(),
            attachments: heartbeats
                .iter()
                .filter_map(|beat| {
                    beat.pane_id
                        .clone()
                        .map(|pane| (pane, beat.instance_id.clone()))
                })
                .collect(),
            beats: heartbeats
                .into_iter()
                .filter_map(|beat| beat.pane_id.map(|pane| (pane, beat.last_seen)))
                .collect(),
        }
    }

    fn observe(&mut self) -> Snapshot {
        let snapshot = self.snapshot();
        for host in &snapshot.census.hosts {
            if self.seen_hosts.insert((host.pid, host.start_token.clone())) {
                self.step(format!("host seen: {}", host.pid));
            }
        }
        for (pane, instance) in &snapshot.attachments {
            if self
                .seen_attachments
                .insert((pane.clone(), instance.clone()))
            {
                self.step(format!("attachment seen: {pane} {instance}"));
            }
        }
        snapshot
    }

    fn evidence(&self) -> String {
        let snapshot = self.snapshot();
        format!("{snapshot:#?}\nsteps: {:?}", self.steps)
    }

    fn wait(
        &mut self,
        label: &str,
        budget: Duration,
        mut ready: impl FnMut(&Snapshot) -> bool,
    ) -> Snapshot {
        let deadline = Instant::now() + budget;
        loop {
            let snapshot = self.observe();
            if ready(&snapshot) {
                self.step(label);
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "{label} timed out: {snapshot:#?}\nsteps: {:?}",
                self.steps
            );
            std::thread::sleep(POLL);
        }
    }

    fn wait_settled(&mut self, budget: Duration) -> Snapshot {
        self.wait_settled_seeing(budget, |_| {})
    }

    /// `wait_settled`, showing `see` every snapshot polled on the way.
    fn wait_settled_seeing(
        &mut self,
        budget: Duration,
        mut see: impl FnMut(&Snapshot),
    ) -> Snapshot {
        let panes = self.panes.clone();
        let mut first: Option<(Proc, HashMap<PaneId, jiff::Timestamp>)> = None;
        self.wait(
            "settled: one host, two attached supervisors, no workers",
            budget,
            |snapshot| {
                see(snapshot);
                if !snapshot.settled(&panes) {
                    first = None;
                    return false;
                }
                // Workers share the supervisor's instance id. Require new beats after
                // workers disappear, rather than accepting their still-fresh last frame.
                if let Some((host, beats)) = &first
                    && host.same_process(&snapshot.census.hosts[0])
                {
                    return panes
                        .iter()
                        .all(|pane| snapshot.beats.get(pane) != beats.get(pane));
                }
                first = Some((snapshot.census.hosts[0].clone(), snapshot.beats.clone()));
                false
            },
        )
    }

    fn kill(&mut self, process: &Proc) -> Duration {
        assert!(
            process.live(),
            "kill target must be live: {process:?}\n{}",
            self.evidence()
        );
        let at = self.step(format!("SIGKILL sent: {}", process.pid));
        kill(Pid::from_raw(process.pid as i32), Signal::SIGKILL).expect("kill ready process");
        self.wait(
            "kill observed (not live, including zombie)",
            Duration::from_secs(5),
            |_| !process.live(),
        );
        at
    }

    fn look(&mut self, index: usize) {
        match &mut self.live {
            LiveRoom::Tmux { socket, .. } => {
                tmux(
                    socket,
                    &["select-window", "-t", &format!("{SESSION}:{index}")],
                );
            }
            LiveRoom::Zellij { cleanup, .. } => {
                let status = cleanup
                    .namespace
                    .command()
                    .args([
                        "--session",
                        SESSION,
                        "action",
                        "go-to-tab",
                        &(index + 1).to_string(),
                    ])
                    .bounded_status()
                    .expect("select tab");
                assert!(status.success(), "go-to-tab failed: {status}");
            }
        }
    }

    fn screen(&mut self, index: usize) -> String {
        match &mut self.live {
            LiveRoom::Tmux { socket, .. } => tmux_capture(
                socket,
                &["capture-pane", "-p", "-t", self.panes[index].raw()],
            ),
            LiveRoom::Zellij { client, .. } => client.contents(),
        }
    }

    fn assert_paints(&mut self, index: usize, marker: Option<&str>) -> String {
        self.look(index);
        let deadline = Instant::now() + CAPTURE_BUDGET;
        loop {
            let screen = self.screen(index);
            let right_tab = !matches!(self.live, LiveRoom::Zellij { .. })
                || screen.lines().any(|line| {
                    line.contains(TAB_MARKERS[index]) && !line.contains(TAB_MARKERS[1 - index])
                });
            if right_tab
                && screen.contains("? for help")
                && screen.contains('─')
                && marker.is_none_or(|marker| screen.contains(marker))
            {
                self.step(format!("pane {} painted {marker:?}", self.panes[index]));
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "pane {} did not paint {marker:?}:\n{screen}\n{}\nevents: {:#?}",
                self.panes[index],
                self.evidence(),
                self.env.read_events()
            );
            std::thread::sleep(POLL);
        }
    }

    fn paint_marker(&mut self, marker: &str, agent_index: usize, indices: &[usize]) {
        for &index in indices {
            let screen = self.assert_paints(index, None);
            assert!(
                !screen.contains(marker),
                "marker must be new: {screen}\n{}",
                self.evidence()
            );
        }
        let pane_var = match self.panes[0].mux() {
            MuxName::Tmux => "TMUX_PANE",
            MuxName::Zellij => "ZELLIJ_PANE_ID",
        };
        let fake_codex = self.env.home_root.join("codex");
        let mut owners: Vec<_> = rimz::proc::list_processes()
            .into_iter()
            .filter(|process| {
                rimz::proc::argv(process.pid).is_some_and(|argv| {
                    argv.first()
                        .is_some_and(|arg| arg.as_os_str() == fake_codex.as_os_str())
                })
            })
            .filter_map(|process| {
                rimz::proc::env_var(process.pid, pane_var).map(|pane| (pane, process.pid))
            })
            .collect();
        owners.sort();
        assert_eq!(
            owners.len(),
            2,
            "marker owners: {owners:?}\n{}",
            self.evidence()
        );
        let (pane, owner) = &owners[agent_index];
        let owner_pid = owner.to_string();
        let output = self.env.run_installed_hook_in_pane(
            "codex",
            &session_start_at(
                &format!("session-{marker}"),
                "GPT-5.5",
                "high",
                self.env.project_root.display().to_string(),
                None,
            )
            .to_string(),
            &[
                ("RIMZ_AGENT_PID", &owner_pid),
                (pane_var, pane),
                (rimz::harness::launch::ENV_AGENT_NAME, marker),
                (rimz::harness::launch::ENV_AGENT_ROLE, marker),
                (rimz::harness::launch::ENV_AGENT_PROFILE, "codex"),
            ],
        );
        assert!(
            output.status.success(),
            "marker hook failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            self.evidence()
        );
        self.step(format!("hook marker fired: {marker}"));
        for &index in indices {
            self.assert_paints(index, Some(marker));
        }
    }
}

fn shape(mux: MuxName) {
    let Some(mut room) = Room::new(mux, false) else {
        return;
    };
    room.wait_settled(CAPTURE_BUDGET);
    for index in 0..2 {
        room.assert_paints(index, None);
    }
    let panes = room.panes.clone();
    room.wait("painted room stays settled", CENSUS_RETRY, |snapshot| {
        snapshot.settled(&panes)
    });
}

fn host_kill(mux: MuxName) {
    let Some(mut room) = Room::new(mux, true) else {
        return;
    };
    // Gated startup has no prior worker heartbeat to outwait.
    let panes = room.panes.clone();
    let initial = room.wait(
        "young initial host attachments",
        CAPTURE_BUDGET,
        |snapshot| snapshot.settled(&panes),
    );
    room.step("young attachments ready");
    let old_host = &initial.census.hosts[0];
    let killed_at = room.kill(old_host);
    assert!(
        killed_at - room.released_at < HOST_KILL_STABLE - HOST_KILL_MARGIN,
        "fixture precondition: young kill has less than {HOST_KILL_MARGIN:?} of birth-bound margin\n{}",
        room.evidence()
    );
    loop {
        let snapshot = room.observe();
        if !snapshot.census.workers.is_empty() {
            room.step("young fallback worker seen");
            break;
        }
        let age_bound = room.t0.elapsed() - room.released_at;
        assert!(
            !snapshot
                .census
                .hosts
                .iter()
                .any(|host| !host.same_process(old_host)),
            "{}: successor without the young worker interlude; supervisor age <= {age_bound:?}\n{snapshot:#?}\nsteps: {:?}",
            if age_bound < HOST_KILL_STABLE {
                "product branch failure"
            } else {
                "fixture precondition: host-loss decision outlasted the young window"
            },
            room.steps
        );
        assert!(
            room.t0.elapsed() - killed_at < CAPTURE_BUDGET,
            "young fallback worker timed out: {snapshot:#?}\nsteps: {:?}",
            room.steps
        );
        std::thread::sleep(POLL);
    }
    let young = room.wait_settled(CAPTURE_BUDGET);
    let reattached_at = room.step("young recovery converged");
    assert!(
        young.census.hosts[0].pid != old_host.pid,
        "young recovery needs a successor: {young:#?}\nsteps: {:?}",
        room.steps
    );
    room.paint_marker("youngpaint", 0, &[0, 1]);
    room.wait(
        "young repaint stays on the successor host",
        CENSUS_RETRY,
        |painted| {
            painted.settled(&panes) && painted.census.hosts[0].same_process(&young.census.hosts[0])
        },
    );

    let mature_at = reattached_at + HOST_KILL_STABLE + HOST_KILL_MARGIN;
    std::thread::sleep(mature_at.saturating_sub(room.t0.elapsed()));
    let old_host = &young.census.hosts[0];
    // Start the census at the signal so the kill-observation poll belongs to the no-worker assertion.
    assert!(
        old_host.live(),
        "mature host must still be live\n{}",
        room.evidence()
    );
    room.step(format!("mature SIGKILL sent: {}", old_host.pid));
    kill(Pid::from_raw(old_host.pid as i32), Signal::SIGKILL).expect("kill mature host");
    let mut workers = Vec::new();
    let mut collect = |snapshot: &Snapshot| workers.extend(snapshot.census.workers.iter().cloned());
    room.wait(
        "mature kill observed (not live, including zombie)",
        Duration::from_secs(5),
        |snapshot| {
            collect(snapshot);
            !old_host.live()
        },
    );
    let mature = room.wait_settled_seeing(CAPTURE_BUDGET, collect);
    assert!(
        workers.is_empty(),
        "mature recovery must never show a worker (attach-clock margin {HOST_KILL_MARGIN:?}): {workers:#?}\n{mature:#?}\nsteps: {:?}",
        room.steps
    );
    assert!(
        mature.census.hosts[0].pid != old_host.pid,
        "mature recovery needs a successor: {mature:#?}\nsteps: {:?}",
        room.steps
    );
    room.paint_marker("maturepaint", 1, &[0, 1]);
    room.wait(
        "mature repaint stays on the successor host",
        CENSUS_RETRY,
        |painted| {
            painted.settled(&panes) && painted.census.hosts[0].same_process(&mature.census.hosts[0])
        },
    );
}

fn supervisor_kill(mux: MuxName) {
    let Some(mut room) = Room::new(mux, false) else {
        return;
    };
    let initial = room.wait_settled(CAPTURE_BUDGET);
    let host = &initial.census.hosts[0];
    let victim = initial
        .census
        .supervisors
        .iter()
        .find(|supervisor| supervisor.pid == host.ppid)
        .unwrap_or_else(|| {
            panic!(
                "no host-spawning supervisor to prove reparenting: {initial:#?}\nsteps: {:?}",
                room.steps
            )
        });
    let pane = victim.pane.as_ref().expect("settled supervisor pane");
    let instance = initial.attachments[pane].clone();
    let victim_index = room
        .panes
        .iter()
        .position(|id| id == pane)
        .expect("victim pane");
    let survivor_pane = room.panes[1 - victim_index].clone();
    room.step(format!("victim {} is host parent", victim.pid));
    room.kill(victim);
    room.wait(
        "killed instance evicted, same host, survivor attachment kept (TTL + 5s)",
        Duration::from_secs(10),
        |remaining| {
            !remaining.attachments.values().any(|id| id == &instance)
                && remaining.census.hosts.len() == 1
                && remaining.census.hosts[0].same_process(host)
                && remaining.census.workers.is_empty()
                && remaining.census.supervisors.len() == 1
                && remaining.attachments.len() == 1
                && remaining.attachments.get(&survivor_pane)
                    == initial.attachments.get(&survivor_pane)
        },
    );
    room.paint_marker("survivorpaint", 1 - victim_index, &[1 - victim_index]);
    room.wait(
        "survivor painted on the original host",
        CENSUS_RETRY,
        |painted| {
            painted.census.hosts.len() == 1
                && painted.census.hosts[0].same_process(host)
                && painted.census.workers.is_empty()
                && painted.census.supervisors.len() == 1
        },
    );
}

#[test]
fn tmux_room_host_one_host_one_supervisor_per_pane_no_worker() {
    shape(MuxName::Tmux);
}

#[test]
fn zellij_room_host_one_host_one_supervisor_per_pane_no_worker() {
    shape(MuxName::Zellij);
}

#[test]
fn tmux_room_host_host_kill_recovers_young_then_mature() {
    host_kill(MuxName::Tmux);
}

#[test]
fn zellij_room_host_host_kill_recovers_young_then_mature() {
    host_kill(MuxName::Zellij);
}

#[test]
fn tmux_room_host_supervisor_kill_drops_pane_others_paint() {
    supervisor_kill(MuxName::Tmux);
}

#[test]
fn zellij_room_host_supervisor_kill_drops_pane_others_paint() {
    supervisor_kill(MuxName::Zellij);
}
