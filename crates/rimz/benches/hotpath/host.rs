use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use divan::Bencher;
use rimz::agents::{AgentContext, AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::{AgentKind, EventId};
use rimz::store::event::EventEnvelope;
use rimz::store::{agent_context, event_log, ingress};
use serde_json::{Value, json};

use super::latency::{emit, scale_rooms, work_records};
use super::{CARRYOVER_PROMPT_BYTES, HookFixture};

const KEY: &str = "host_scale";
const SESSION: &str = "scale";
const CLOSED: usize = 300;
const WORKING: usize = 25;
const THREADS: usize = 16;
const WARMUP: Duration = Duration::from_secs(60);
const WINDOW: Duration = Duration::from_secs(120);
const DEADLINE: Duration = Duration::from_secs(120);

// The same bench executable supplies parked agents and exec-only PATH shims.
// JSON argv keeps tmux's quoted commands intact without forking a logging helper.
pub(super) fn helper() {
    let executable = std::env::args().next().unwrap();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let name = Path::new(&executable)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    if matches!(name, "tmux" | "git") {
        let mut line = serde_json::to_vec(&json!({
            "ppid": nix::unistd::getppid().as_raw(), "argv": args,
            "at_ms": jiff::Timestamp::now().as_millisecond(),
        }))
        .unwrap();
        line.push(b'\n');
        let root = PathBuf::from(std::env::var_os("RIMZ_SCALE_SPAWN_DIR").unwrap());
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join(format!("{name}.jsonl")))
            .unwrap();
        assert_eq!(log.write(&line).unwrap(), line.len());
        drop(log);
        let real = std::env::var_os(format!("RIMZ_SCALE_REAL_{}", name.to_uppercase())).unwrap();
        panic!("exec {name}: {}", Command::new(real).args(args).exec());
    }
    if args == ["--host-scale-agent"] {
        for _ in 1..THREADS {
            thread::spawn(|| {
                loop {
                    thread::park();
                }
            });
        }
        loop {
            thread::park();
        }
    }
    if name == "claude" {
        if args == ["--version"] {
            writeln!(io::stdout().lock(), "2.1.83 (Claude Code bench stand-in)").unwrap();
            std::process::exit(0);
        }
        assert_eq!(args, ["auth", "status"], "unexpected provider probe");
        writeln!(io::stdout().lock(), "{}", json!({"loggedIn": false})).unwrap();
        std::process::exit(1);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostStart {
    Prestarted,
    Supervisor,
    Traced,
}

impl HostStart {
    fn name(self) -> &'static str {
        match self {
            Self::Prestarted => "prestarted",
            Self::Supervisor => "supervisor",
            Self::Traced => "traced",
        }
    }
}

struct Pane {
    id: String,
    pid: u32,
}

struct Room {
    roots: HookFixture,
    binary: PathBuf,
    project: PathBuf,
    worktrees: Vec<PathBuf>,
    env: BTreeMap<String, String>,
    store: rimz::Store,
    tmux: PathBuf,
    socket: PathBuf,
    agents: Vec<Pane>,
    sidebars: Vec<Pane>,
    children: Vec<Child>,
    host: u32,
    server: u32,
    start: HostStart,
}

fn checked(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < DEADLINE, "timed out: {description}");
        thread::sleep(Duration::from_millis(100));
    }
}

impl Room {
    fn new(agents: usize, sidebars: usize, start: HostStart) -> Result<Self, String> {
        assert!(std::env::var_os("RIMZ_HOOK_BUDGET_HOME").is_none());
        let roots = HookFixture::new();
        let binary = std::env::var_os("RIMZ_HOOK_BUDGET_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| roots.binary.clone())
            .canonicalize()
            .expect("CLI binary; build release with --features testkit --bins");
        let tmux = which::which("tmux").expect("host_scale requires tmux");
        let git = which::which("git").expect("host_scale requires git");
        let root = roots.home_root.path();
        let home = root.join("state");
        let project = root.join("repo");
        fs::create_dir(&project).unwrap();
        let paths = rimz::StatePaths::for_project_root_under(&project, &home).unwrap();
        let runtime = rimz::RuntimePaths::for_state_under(&paths, roots.runtime_root.path());
        let socket = rimz::mux::tmux::managed_server_socket_path_under(roots.runtime_root.path());
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let mut env = BTreeMap::new();
        for (key, path) in [
            ("HOME", root.to_path_buf()),
            ("RIMZ_HOME", home.clone()),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_DATA_HOME", root.join("data")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("XDG_STATE_HOME", root.join("xdg-state")),
            ("CLAUDE_CONFIG_DIR", root.join("claude")),
            ("CODEX_HOME", root.join("codex")),
            ("TMPDIR", root.join("tmp")),
            ("TMUX_TMPDIR", roots.runtime_root.path().into()),
            ("XDG_RUNTIME_DIR", roots.runtime_root.path().into()),
            ("RIMZ_SCALE_SPAWN_DIR", root.join("spawns")),
        ] {
            fs::create_dir_all(&path).unwrap();
            env.insert(key.to_owned(), path.to_str().unwrap().to_owned());
        }
        let shims = root.join("bin");
        fs::create_dir(&shims).unwrap();
        for name in ["tmux", "git", "claude"] {
            std::os::unix::fs::symlink(std::env::current_exe().unwrap(), shims.join(name)).unwrap();
            fs::write(root.join("spawns").join(format!("{name}.jsonl")), b"").unwrap();
        }
        fs::write(root.join("work.jsonl"), b"").unwrap();
        env.extend([
            ("PATH".into(), format!("{}:/usr/bin:/bin", shims.display())),
            ("SHELL".into(), "/bin/sh".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("RIMZ_WORKSPACE_ID".into(), paths.workspace_id.to_string()),
            ("RIMZ_PROJECT_ROOT".into(), project.to_str().unwrap().into()),
            ("RIMZ_BIN".into(), binary.to_str().unwrap().into()),
            ("RIMZ_RUNTIME_ENV".into(), "1".into()),
            (
                "RIMZ_CODEX_BIN".into(),
                "/nonexistent/rimz-fixture-codex".into(),
            ),
            ("RIMZ_OAUTH_USAGE_OFFLINE".into(), "1".into()),
            ("RIMZ_SCALE_REAL_TMUX".into(), tmux.to_str().unwrap().into()),
            ("RIMZ_SCALE_REAL_GIT".into(), git.to_str().unwrap().into()),
            (
                "RIMZ_TEST_WORK_LOG".into(),
                root.join("work.jsonl").to_str().unwrap().into(),
            ),
            ("RIMZ_TEST_HOOK_REPLY_WAIT_MS".into(), "30000".into()),
        ]);
        let store = rimz::Store::open(paths, runtime).unwrap();
        let mut room = Self {
            roots,
            binary,
            worktrees: vec![project.clone()],
            project,
            env,
            store,
            tmux,
            socket,
            agents: Vec::new(),
            sidebars: Vec::new(),
            children: Vec::new(),
            host: 0,
            server: 0,
            start,
        };
        for args in [
            &["init", "--quiet"][..],
            &[
                "-c",
                "user.name=RimZ bench",
                "-c",
                "user.email=bench@invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture",
            ][..],
        ] {
            checked(room.command(&git).args(args));
        }
        for index in 1..sidebars {
            let worktree = room
                .roots
                .home_root
                .path()
                .join("worktrees")
                .join(format!("window-{index}"));
            checked(
                room.command(&git)
                    .args(["worktree", "add", "--quiet", "-b"])
                    .arg(format!("scale-{index}"))
                    .arg(&worktree),
            );
            room.worktrees.push(worktree);
        }
        let mut resolved = rimz::WorkspaceResolver::resolve_under(
            &room.project,
            Some(room.project.clone()),
            &home,
        )
        .unwrap();
        assert_eq!(resolved.root_class, rimz::workspace::RootClass::Repo);
        resolved.session_name = SESSION.into();
        room.store.record_workspace(&resolved).unwrap();
        checked(
            room.command(&room.binary)
                .args(["hooks", "install", "claude"]),
        );
        room.seed_history();
        room.create_agents(agents, sidebars);
        room.feed(0);
        if start != HostStart::Supervisor {
            room.start_host()?;
        }
        room.create_sidebars(sidebars);
        let attach = format!(
            "unset TMUX TMUX_PANE; exec {} -S {} attach-session -t {SESSION}",
            quote(room.tmux.to_str().unwrap()),
            quote(room.socket.to_str().unwrap()),
        );
        checked(room.command(&room.tmux).args([
            "-L",
            "scale-carrier",
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "200",
            "-y",
            "50",
            &attach,
        ]));
        wait_for("carrier's native client attached", || {
            !room
                .tmux(&["list-clients", "-F", "#{client_pid}"])
                .is_empty()
        });
        wait_for("all sidebar attachments painted", || room.attached());
        if start == HostStart::Supervisor {
            let hosts: BTreeSet<_> = rimz::proc::list_processes()
                .into_iter()
                .filter(|info| {
                    info.cmdline
                        .contains(room.store.paths().workspace_id.as_str())
                })
                .filter(|info| {
                    rimz::proc::argv(info.pid)
                        .is_some_and(|args| args.windows(2).any(|pair| pair == ["sidebar", "host"]))
                })
                .map(|info| info.pid)
                .collect();
            assert_eq!(hosts.len(), 1, "one supervisor-started host: {hosts:?}");
            room.host = *hosts.first().unwrap();
        }
        room.server = room
            .tmux(&["display-message", "-p", "#{pid}"])
            .parse()
            .unwrap();
        room.assert_shape();
        emit(
            KEY,
            json!({"kind": "room", "agents": agents, "sidebars": sidebars,
            "closed": CLOSED, "working": WORKING, "threads_per_agent": THREADS,
            "worktrees": room.worktrees.len(), "hot_worktrees": WORKING.min(sidebars),
            "build_id": rimz::build_id::of_file(&room.binary).unwrap(),
            "version": checked(room.command(&room.binary).arg("--version")),
            "traced": start == HostStart::Traced, "host_start": start.name()}),
        );
        Ok(room)
    }

    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn tmux(&self, args: &[&str]) -> String {
        checked(
            self.command(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .args(args),
        )
    }

    fn seed_history(&self) {
        rimz::testkit::fleet::seed_history_carryover(&self.store, CLOSED, CARRYOVER_PROMPT_BYTES)
            .unwrap();
        for index in 0..CLOSED {
            let id = format!("history-{index}");
            let observation =
                AgentLifecycleObservation::new(Some(id.clone().into()), LifecycleSignal::Ended);
            event_log::append(
                &self.store.paths().events_log,
                &EventEnvelope::agent_lifecycle(
                    self.store.paths().workspace_id.clone(),
                    id,
                    "claude",
                    "SessionEnd",
                    &observation,
                ),
            )
            .unwrap();
        }
    }

    fn create_agents(&mut self, count: usize, sidebars: usize) {
        let agent = format!(
            "exec {} --host-scale-agent",
            quote(
                self.roots
                    .home_root
                    .path()
                    .join("bin/claude")
                    .to_str()
                    .unwrap()
            )
        );
        for index in 0..count {
            let window = index % sidebars;
            let worktree = self.worktrees[window].to_str().unwrap();
            let target = format!("{SESSION}:{window}");
            let line = if index == 0 {
                self.tmux(&[
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}\t#{pane_pid}",
                    "-s",
                    SESSION,
                    "-x",
                    "200",
                    "-y",
                    "50",
                    "-c",
                    worktree,
                    &agent,
                ])
            } else if index < sidebars {
                self.tmux(&[
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}\t#{pane_pid}",
                    "-t",
                    &target,
                    "-c",
                    worktree,
                    &agent,
                ])
            } else {
                let first = &self.agents[window].id;
                let line = self.tmux(&[
                    "split-window",
                    "-d",
                    "-h",
                    "-l",
                    "1",
                    "-P",
                    "-F",
                    "#{pane_id}\t#{pane_pid}",
                    "-t",
                    first,
                    "-c",
                    worktree,
                    &agent,
                ]);
                self.tmux(&["select-layout", "-t", &target, "tiled"]);
                line
            };
            let (id, pid) = line.split_once('\t').expect("pane id and pid");
            self.agents.push(Pane {
                id: id.into(),
                pid: pid.parse().unwrap(),
            });
            wait_for("agent's sixteen parked threads", || {
                fs::read_dir(format!("/proc/{pid}/task"))
                    .is_ok_and(|entries| entries.count() == THREADS)
            });
            self.apply(index, "SessionStart", json!({"source": "startup"}));
            self.apply(
                index,
                "UserPromptSubmit",
                json!({"prompt": "check the repository"}),
            );
            self.apply(index, "Stop", json!({"last_assistant_message": "done"}));
            agent_context::write(
                self.store.runtime_paths(),
                "claude",
                &format!("scale-{index}"),
                &AgentContext::new("claude", jiff::Timestamp::now()),
                None,
            )
            .unwrap();
        }
        for index in 0..WORKING {
            self.apply(
                index,
                "UserPromptSubmit",
                json!({"prompt": "check the repository"}),
            );
        }
    }

    fn payload(&self, index: usize, event: &str, mut payload: Value) -> Value {
        payload["hook_event_name"] = json!(event);
        payload["session_id"] = json!(format!("scale-{index}"));
        payload["cwd"] = json!(self.worktrees[index % self.worktrees.len()]);
        payload
    }

    fn agent_command(&self, index: usize) -> Command {
        let mut command = self.command(&self.binary);
        command
            .current_dir(&self.worktrees[index % self.worktrees.len()])
            .env("RIMZ_AGENT_PID", self.agents[index].pid.to_string())
            .env("TMUX_PANE", &self.agents[index].id)
            .stdin(Stdio::piped());
        command
    }

    fn send(&self, command: &mut Command, payload: &impl serde::Serialize) {
        let mut child = command.spawn().unwrap();
        serde_json::to_writer(child.stdin.take().unwrap(), payload).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{command:?}: {output:?}");
    }

    fn apply(&self, index: usize, event: &str, payload: Value) {
        let mut env = self.env.clone();
        env.insert("RIMZ_AGENT_PID".into(), self.agents[index].pid.to_string());
        env.insert("TMUX_PANE".into(), self.agents[index].id.clone());
        let frame = ingress::HookIngress {
            boot_id: rimz::proc::boot_id(),
            schema_version: "1".into(),
            event_id: EventId::new(),
            ts: jiff::Timestamp::now(),
            source: AgentKind::new_unchecked("claude"),
            event: Some(event.into()),
            payload: self.payload(index, event, payload).to_string(),
            cwd: self.worktrees[index % self.worktrees.len()].clone(),
            hook_pid: std::process::id(),
            env,
        };
        self.send(self.agent_command(index).args(["hooks", "apply"]), &frame);
    }

    fn start_host(&mut self) -> Result<(), String> {
        let stderr = self.roots.home_root.path().join("host.stderr");
        let mut command = if self.start == HostStart::Traced {
            let strace =
                which::which("strace").map_err(|error| format!("strace unavailable: {error}"))?;
            let mut command = self.command(&strace);
            command
                // Stop only at opens and thread creation, not at every syscall.
                .args(["-ff", "--seccomp-bpf", "-ttt", "-s", "4096"])
                .args(["-e", "trace=openat,clone,clone3", "-o"])
                .arg(self.roots.home_root.path().join("opens"))
                .arg(&self.binary);
            command
        } else {
            self.command(&self.binary)
        };
        command
            .args([
                "sidebar",
                "host",
                "--mux",
                "tmux",
                "--workspace-id",
                self.store.paths().workspace_id.as_str(),
                "--session-name",
                SESSION,
            ])
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).unwrap());
        self.children.push(
            command
                .spawn()
                .map_err(|error| format!("start host: {error}"))?,
        );
        let since = Instant::now();
        loop {
            let pid = if self.start == HostStart::Traced {
                rimz::proc::children(self.children[0].id()).first().copied()
            } else {
                Some(self.children[0].id())
            };
            if let Some(pid) = pid {
                // The socket is private to this room; readiness needs no synthetic attachment.
                let listening = fs::read_dir(&self.store.runtime_paths().sock_dir)
                    .unwrap()
                    .filter_map(Result::ok)
                    .any(|entry| {
                        entry.file_name().to_string_lossy().starts_with("host.")
                            && entry.path().extension().is_some_and(|ext| ext == "sock")
                    });
                if listening {
                    self.host = pid;
                    return Ok(());
                }
            }
            if let Some(exit) = self.children[0].try_wait().unwrap() {
                return Err(format!(
                    "host exited {exit}: {}",
                    fs::read_to_string(&stderr).unwrap()
                ));
            }
            assert!(
                since.elapsed() < DEADLINE,
                "host socket startup: {}",
                fs::read_to_string(&stderr).unwrap()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn create_sidebars(&mut self, count: usize) {
        let serve = format!(
            "exec {} sidebar serve --mux tmux --workspace-id {} \
             --session-name {SESSION} --tick-seconds 1",
            quote(self.binary.to_str().unwrap()),
            self.store.paths().workspace_id
        );
        for index in 0..count {
            let line = self.tmux(&[
                "split-window",
                "-d",
                "-f",
                "-h",
                "-b",
                "-l",
                "62",
                "-P",
                "-F",
                "#{pane_id}\t#{pane_pid}",
                "-t",
                &self.agents[index].id,
                &serve,
            ]);
            let (id, pid) = line.split_once('\t').unwrap();
            self.sidebars.push(Pane {
                id: id.into(),
                pid: pid.parse().unwrap(),
            });
        }
    }

    fn attached(&self) -> bool {
        let heartbeats = rimz::sidebar::live_sidebars(self.store.runtime_paths());
        heartbeats.len() == self.sidebars.len()
            && self.sidebars.iter().all(|pane| {
                heartbeats.iter().any(|sidebar| {
                    let beat = &sidebar.heartbeat;
                    beat.size.is_some()
                        && beat.session_name == SESSION
                        && beat
                            .pane_id
                            .as_ref()
                            .is_some_and(|id| id.as_str() == format!("tmux:{}", pane.id))
                })
            })
    }

    fn assert_shape(&self) {
        assert!(
            self.attached(),
            "P4: every sidebar must be attached and painting"
        );
        let snapshot = self.store.snapshot_cached().unwrap();
        assert_eq!(
            fs::read_dir(&self.store.runtime_paths().agent_context_dir)
                .unwrap()
                .count(),
            self.agents.len()
        );
        assert_eq!(
            snapshot.agents.len(),
            self.agents.len(),
            "P2: live agent count"
        );
        assert_eq!(snapshot.fenced_sessions.len(), CLOSED);
        for (index, pane) in self.agents.iter().enumerate() {
            let id = format!("scale-{index}");
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.agent_id.as_str() == id)
                .unwrap();
            assert!(agent.ended_at.is_none(), "P2: {id} ended");
            assert_eq!(
                agent.pane.as_ref().unwrap().pane_id.as_str(),
                format!("tmux:{}", pane.id)
            );
            assert_eq!(agent.runtime_owner.as_ref().unwrap().pid, pane.pid);
            assert!(
                rimz::proc::process_is_live(pane.pid, None),
                "P2: {id} owner died"
            );
            assert_eq!(
                fs::read_dir(format!("/proc/{}/task", pane.pid))
                    .unwrap()
                    .count(),
                THREADS
            );
        }
    }

    fn warm(&self) {
        thread::sleep(WARMUP);
        self.assert_shape();
        assert!(
            self.spawn_records("tmux")
                .iter()
                .any(|record| record["ppid"] == self.host),
            "P3: shim PATH must reach the host"
        );
        let git = self.spawn_records("git");
        for worktree in &self.worktrees {
            assert!(
                git.iter().any(|record| {
                    record["ppid"] == self.host
                        && record["argv"]
                            .as_array()
                            .unwrap()
                            .windows(2)
                            .any(|pair| pair[0] == "-C" && pair[1] == worktree.to_str().unwrap())
                }),
                "host git lane must run in {} during warm-up",
                worktree.display()
            );
        }
    }

    fn spawn_records(&self, endpoint: &str) -> Vec<Value> {
        work_records(
            &Path::new(&self.env["RIMZ_SCALE_SPAWN_DIR"]).join(format!("{endpoint}.jsonl")),
        )
    }

    fn measure(&self, phase: &str, duration: Duration) {
        self.assert_shape();
        let before = self.samples();
        let started_ms = jiff::Timestamp::now().as_millisecond();
        let started = Instant::now();
        let mut sent = 0;
        let mut max_lag = Duration::ZERO;
        while started.elapsed() < duration {
            if phase == "busy" && sent < duration.as_secs() * 2 {
                let due = started + Duration::from_millis(sent * 500);
                if Instant::now() >= due {
                    max_lag = max_lag.max(due.elapsed());
                    self.feed(sent as usize);
                    sent += 1;
                    continue;
                }
            }
            let next = if phase == "busy" && sent < duration.as_secs() * 2 {
                started + Duration::from_millis(sent * 500)
            } else {
                started + duration
            };
            thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        let seconds = started.elapsed().as_secs_f64();
        let ended_ms = jiff::Timestamp::now().as_millisecond();
        let after = self.samples();
        let per_minute = 60.0 / seconds;
        let records: Vec<_> = work_records(Path::new(&self.env["RIMZ_TEST_WORK_LOG"]))
            .into_iter()
            .filter(|record| {
                let ended = record["ended_ms"].as_i64().unwrap();
                ended >= started_ms && ended < ended_ms && record["command"] != "hooks feed"
            })
            .collect();
        let cpu_us: u64 = records
            .iter()
            .map(|record| record["user_us"].as_u64().unwrap() + record["sys_us"].as_u64().unwrap())
            .sum();
        let deltas: BTreeMap<_, _> = after
            .iter()
            .map(|(&pid, sample)| (pid, sample.delta(&before[&pid])))
            .collect();
        let serve_sum = |field: fn(&Delta) -> u64| -> u64 {
            self.sidebars
                .iter()
                .map(|pane| field(&deltas[&pane.pid]))
                .sum()
        };
        let host = &deltas[&self.host];
        let ticks_ms = 1000.0 / rimz::proc::clk_tck() as f64 * per_minute;
        let mut spawns = BTreeMap::<String, BTreeMap<String, BTreeMap<String, f64>>>::new();
        for endpoint in ["tmux", "git"] {
            for record in self.spawn_records(endpoint).into_iter().filter(|record| {
                let stamp = record["at_ms"].as_i64().unwrap();
                stamp >= started_ms && stamp < ended_ms
            }) {
                let parent = self.parent(record["ppid"].as_u64().unwrap() as u32);
                let args: Vec<_> = record["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|arg| arg.as_str().unwrap())
                    .collect();
                let command = subcommand(endpoint, &args);
                *spawns
                    .entry(parent.into())
                    .or_default()
                    .entry(endpoint.into())
                    .or_default()
                    .entry(command)
                    .or_default() += per_minute;
            }
        }
        for record in &records {
            *spawns
                .entry(self.parent(record["ppid"].as_u64().unwrap() as u32).into())
                .or_default()
                .entry("rimz".into())
                .or_default()
                .entry(record["command"].as_str().unwrap().into())
                .or_default() += per_minute;
        }
        self.assert_shape();
        if phase == "busy" {
            assert_eq!(
                sent,
                duration.as_secs() * 2,
                "busy feed missed its schedule"
            );
        }
        emit(
            KEY,
            json!({"kind": "window", "agents": self.agents.len(), "sidebars": self.sidebars.len(),
                "phase": phase, "host_start": self.start.name(), "seconds": seconds,
                "frames_sent": sent,
                "max_send_lag_ms": max_lag.as_secs_f64() * 1000.0,
                "host_threads": after[&self.host].switches.len(),
                "host_attach_threads": host.attach_threads,
                "cpu_ms_per_minute": {"host": host.cpu as f64 * ticks_ms,
                    "host_attach": host.attach_cpu as f64 * ticks_ms,
                    "serve": serve_sum(|delta| delta.cpu) as f64 * ticks_ms,
                    "tmux_server": deltas[&self.server].cpu as f64 * ticks_ms,
                    "host_reaped_children": host.children as f64 * ticks_ms,
                    "rimz_children": cpu_us as f64 / 1000.0 * per_minute},
                "wakeups_per_minute": {"host": host.switches as f64 * per_minute,
                    "host_attach": host.attach_switches as f64 * per_minute,
                    "serve": serve_sum(|delta| delta.switches) as f64 * per_minute},
                "wakeups_scope": "boundary task deltas; exited tasks are not included",
                "departed_tasks": {"host": host.departed,
                    "serve": serve_sum(|delta| delta.departed)},
                "read_syscalls_per_minute": {"host": host.syscr as f64 * per_minute,
                    "serve": serve_sum(|delta| delta.syscr) as f64 * per_minute},
                "read_bytes_per_minute": {"host": host.rchar as f64 * per_minute,
                    "serve": serve_sum(|delta| delta.rchar) as f64 * per_minute},
                "read_bytes_source": "rchar",
                "rimz_children_scope": "completed in window; hooks feed excluded",
                "spawns_per_minute": spawns, "folds_per_minute": self.folds(started_ms, ended_ms),
            }),
        );
    }

    fn feed(&self, sequence: usize) {
        let sender = sequence % WORKING;
        let offset = (sequence / WORKING + sender) % 20;
        let (event, payload) = match offset {
            0 => (
                "UserPromptSubmit",
                json!({"prompt": "check the repository"}),
            ),
            19 => ("Stop", json!({"last_assistant_message": "done"})),
            _ => (
                "PostToolUse",
                json!({"tool_name": "Read", "tool_input": {}, "tool_response": {}}),
            ),
        };
        self.send(
            self.agent_command(sender)
                .args(["hooks", "feed", "--source", "claude", "--event", event]),
            &self.payload(sender, event, payload),
        );
    }

    fn parent(&self, pid: u32) -> &'static str {
        if pid == self.host {
            "host"
        } else if self.sidebars.iter().any(|pane| pane.pid == pid) {
            "serve"
        } else {
            "other"
        }
    }

    fn samples(&self) -> BTreeMap<u32, Sample> {
        [self.host, self.server]
            .into_iter()
            .chain(self.sidebars.iter().map(|pane| pane.pid))
            .map(|pid| (pid, Sample::read(pid)))
            .collect()
    }

    fn folds(&self, started_ms: i64, ended_ms: i64) -> Value {
        let path = rimz::diag::DiagSink::under(
            self.store.paths().root.clone(),
            self.store.paths().workspace_id.clone(),
            SESSION,
            None,
        )
        .log_path()
        .unwrap();
        let records: Vec<_> = fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|record| {
                let ended = record["at_ms"].as_i64().unwrap();
                ended >= started_ms
                    && ended < ended_ms
                    && record["event"]["kind"] == "fetch_cycle_stats"
            })
            .collect();
        let seconds: f64 = records
            .iter()
            .map(|record| record["event"]["interval_ms"].as_f64().unwrap() / 1000.0)
            .sum();
        assert!(seconds > 0.0, "no complete fetch diagnostic window");
        // A cell whose intervals cover under 90% of the phase reads like a whole one
        // unless it is marked, so it carries a flag and warns on stderr.
        let phase_seconds = (ended_ms - started_ms) as f64 / 1000.0;
        let partial = seconds < phase_seconds * 0.9;
        if partial {
            writeln!(
                io::stderr(),
                "warning: fold intervals cover {seconds:.1}s of a {phase_seconds:.1}s phase; \
                 folds_per_minute is partial"
            )
            .expect("benchmark warning");
        }
        let mut result = json!({"covered_seconds": seconds, "phase_seconds": phase_seconds,
            "partial_coverage": partial,
            "scope": "diagnostic intervals ending in window; can cross phase boundary"});
        for name in [
            "cycles",
            "fold_ms",
            "content_reductions",
            "unchanged_reductions",
        ] {
            result[name] = json!(
                records
                    .iter()
                    .map(|record| record["event"][name].as_f64().unwrap())
                    .sum::<f64>()
                    * 60.0
                    / seconds
            );
        }
        result
    }

    fn trace(&self) {
        self.assert_shape();
        let before: BTreeSet<_> = Sample::read(self.host).switches.into_keys().collect();
        let started_ms = jiff::Timestamp::now().as_millisecond();
        let started = Instant::now();
        // Two minutes, so a fetch diagnostic window ends inside it and the traced
        // host's cycle rate shows whether tracing slowed it.
        thread::sleep(WINDOW);
        let seconds = started.elapsed().as_secs_f64();
        let ended_ms = jiff::Timestamp::now().as_millisecond();
        self.assert_shape();
        let after: BTreeSet<_> = Sample::read(self.host).switches.into_keys().collect();
        let tasks = self.trace_threads();
        assert!(
            before.is_subset(&tasks) && after.is_subset(&tasks),
            "trace must cover every boundary-live host thread"
        );
        let mut proc = BTreeMap::<String, f64>::new();
        let mut files = BTreeMap::<String, f64>::new();
        for tid in &tasks {
            let path = self.roots.home_root.path().join(format!("opens.{tid}"));
            for line in io::BufReader::new(fs::File::open(path).unwrap()).lines() {
                let line = line.unwrap();
                let Some((stamp, call)) = line.split_once(' ') else {
                    continue;
                };
                let Ok(stamp) = stamp.parse::<f64>() else {
                    continue;
                };
                if stamp * 1000.0 < started_ms as f64
                    || stamp * 1000.0 >= ended_ms as f64
                    || !call.starts_with("openat(")
                {
                    continue;
                }
                let Some(quote) = call.find('"') else {
                    continue;
                };
                let path = serde_json::Deserializer::from_str(&call[quote..])
                    .into_iter::<String>()
                    .next()
                    .unwrap()
                    .expect("strace quoted filename");
                if path == "/proc" || path.starts_with("/proc/") {
                    let last = path.rsplit('/').next().unwrap();
                    let kind = match last {
                        "children" | "stat" | "io" | "status" | "task" => last,
                        _ if path.contains("/task/") => "task",
                        _ => "other",
                    };
                    *proc.entry(kind.into()).or_default() += 60.0 / seconds;
                } else {
                    *files.entry(self.path_class(&path)).or_default() += 60.0 / seconds;
                }
            }
        }
        assert!(!proc.is_empty(), "trace captured no host /proc opens");
        emit(
            KEY,
            json!({"kind": "trace", "agents": self.agents.len(), "sidebars": self.sidebars.len(),
            "seconds": seconds, "proc_opens_per_minute": proc, "file_opens_per_minute": files,
            "counts": "openat attempts, including failures; host CLONE_THREAD lineage only",
            "host_thread_files": tasks.len(),
            "folds_per_minute": self.folds(started_ms, ended_ms)}),
        );
    }

    fn trace_threads(&self) -> BTreeSet<u32> {
        let mut tasks = BTreeSet::new();
        let mut pending = vec![self.host];
        while let Some(tid) = pending.pop() {
            if !tasks.insert(tid) {
                continue;
            }
            let path = self.roots.home_root.path().join(format!("opens.{tid}"));
            let mut thread_clone = false;
            for line in io::BufReader::new(fs::File::open(path).unwrap()).lines() {
                let line = line.unwrap();
                let Some((_, call)) = line.split_once(' ') else {
                    continue;
                };
                if call.starts_with("clone(") || call.starts_with("clone3(") {
                    thread_clone = call.contains("CLONE_THREAD");
                } else if !call.starts_with("<... clone resumed>")
                    && !call.starts_with("<... clone3 resumed>")
                {
                    continue;
                }
                let Some((_, result)) = call.rsplit_once(" = ") else {
                    continue;
                };
                if thread_clone
                    && let Some(child) = result
                        .split_whitespace()
                        .next()
                        .and_then(|pid| pid.parse::<u32>().ok())
                        .filter(|&pid| pid > 0)
                {
                    pending.push(child);
                }
                thread_clone = false;
            }
        }
        tasks
    }

    fn path_class(&self, path: &str) -> String {
        let path = path
            .replace(self.store.paths().root.to_str().unwrap(), "<state>")
            .replace(
                self.store
                    .paths()
                    .root
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "<workspace>",
            )
            .replace(self.store.paths().workspace_id.as_str(), "<workspace>")
            .replace(self.roots.home_root.path().to_str().unwrap(), "<home>")
            .replace(
                self.roots.runtime_root.path().to_str().unwrap(),
                "<runtime>",
            );
        path.split('/')
            .map(normalize_component)
            .collect::<Vec<_>>()
            .join("/")
    }

    fn stop_muxes(&self) {
        let _ = self
            .command(&self.tmux)
            .args(["-L", "scale-carrier", "kill-server"])
            .output();
        let _ = self
            .command(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }

    fn finish(mut self) {
        self.stop_muxes();
        wait_for("host returned after its last attachment", || {
            !rimz::proc::process_is_live(self.host, None)
        });
        for child in &mut self.children {
            assert!(child.wait().unwrap().success());
        }
        let records = work_records(Path::new(&self.env["RIMZ_TEST_WORK_LOG"]));
        let host: Vec<_> = records
            .iter()
            .filter(|record| record["pid"] == self.host)
            .collect();
        assert_eq!(host.len(), 1, "one returned host work record");
        if self.start != HostStart::Traced {
            emit(
                KEY,
                json!({"kind": "work", "agents": self.agents.len(), "sidebars": self.sidebars.len(),
                "host_start": self.start.name(),
                "scope": "whole host lifetime, including warmup", "host": host[0]}),
            );
        }
        rimz::testkit::sandbox::reap(self.roots._sandbox.spec());
        wait_for("all room processes reaped", || {
            rimz::testkit::sandbox::sandbox_processes(self.roots._sandbox.spec()).is_empty()
        });
    }
}

fn normalize_component(part: &str) -> String {
    if part.len() >= 12 && part.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return "<id>".into();
    }
    for separator in ['.', '_', '-'] {
        if let Some((left, right)) = part.split_once(separator) {
            return format!(
                "{}{separator}{}",
                normalize_component(left),
                normalize_component(right)
            );
        }
    }
    let mut result = String::new();
    let mut digits = false;
    for c in part.chars() {
        if c.is_ascii_digit() {
            if !digits {
                result.push_str("<n>");
            }
            digits = true;
        } else {
            result.push(c);
            digits = false;
        }
    }
    result
}

impl Drop for Room {
    fn drop(&mut self) {
        self.stop_muxes();
        rimz::testkit::sandbox::reap(self.roots._sandbox.spec());
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Sample {
    start: u64,
    cpu: u64,
    children: u64,
    syscr: u64,
    rchar: u64,
    switches: BTreeMap<u32, u64>,
    attach: BTreeMap<u32, (u64, u64)>,
}

struct Delta {
    cpu: u64,
    children: u64,
    syscr: u64,
    rchar: u64,
    switches: u64,
    departed: u64,
    attach_threads: usize,
    attach_cpu: u64,
    attach_switches: u64,
}

fn value(text: &str, key: &str) -> u64 {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| (name == key).then(|| value.trim().parse().unwrap()))
        .unwrap_or_else(|| panic!("missing /proc field {key}"))
}

/// A task that exits after its directory is listed reads as ENOENT, or as ESRCH once its
/// file is open.
fn task_exited(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32)
}

fn subcommand(endpoint: &str, args: &[&str]) -> String {
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        let takes_value = match endpoint {
            "tmux" => matches!(arg, "-S" | "-L" | "-f" | "-c"),
            "git" => matches!(arg, "-C" | "-c" | "--git-dir" | "--work-tree"),
            _ => unreachable!("only tmux and git have PATH shims"),
        };
        if takes_value {
            args.next();
        } else if !arg.starts_with('-') {
            return arg.into();
        }
    }
    "<options>".into()
}

impl Sample {
    fn read(pid: u32) -> Self {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        let field = |index: usize| fields[index].parse::<u64>().unwrap();
        let io = fs::read_to_string(format!("/proc/{pid}/io")).unwrap();
        let mut attach = BTreeMap::new();
        let switches = fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let tid: u32 = entry.file_name().to_str().unwrap().parse().unwrap();
                let status = match fs::read_to_string(entry.path().join("status")) {
                    Ok(status) => status,
                    Err(error) if task_exited(&error) => return None,
                    Err(error) => panic!("read task {tid} status: {error}"),
                };
                let switches = value(&status, "voluntary_ctxt_switches")
                    + value(&status, "nonvoluntary_ctxt_switches");
                if status
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .any(|(key, value)| key == "Name" && value.trim() == "rimz-attach")
                {
                    let stat = match fs::read_to_string(entry.path().join("stat")) {
                        Ok(stat) => stat,
                        Err(error) if task_exited(&error) => return None,
                        Err(error) => panic!("read task {tid} stat: {error}"),
                    };
                    let fields: Vec<_> = stat
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .split_whitespace()
                        .collect();
                    let cpu =
                        fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
                    attach.insert(tid, (cpu, switches));
                }
                Some((tid, switches))
            })
            .collect();
        Self {
            start: field(19),
            cpu: field(11) + field(12),
            children: field(13) + field(14),
            syscr: value(&io, "syscr"),
            rchar: value(&io, "rchar"),
            switches,
            attach,
        }
    }

    fn delta(&self, before: &Self) -> Delta {
        assert_eq!(
            self.start, before.start,
            "room process replaced during measurement"
        );
        Delta {
            attach_threads: self
                .attach
                .keys()
                .filter(|tid| before.attach.contains_key(tid))
                .count(),
            attach_cpu: self
                .attach
                .iter()
                .filter_map(|(tid, (cpu, _))| before.attach.get(tid).map(|(prior, _)| cpu - prior))
                .sum(),
            attach_switches: self
                .attach
                .iter()
                .filter_map(|(tid, (_, switches))| {
                    before.attach.get(tid).map(|(_, prior)| switches - prior)
                })
                .sum(),
            cpu: self.cpu - before.cpu,
            children: self.children - before.children,
            syscr: self.syscr - before.syscr,
            rchar: self.rchar - before.rchar,
            switches: self
                .switches
                .iter()
                .map(|(tid, count)| count - before.switches.get(tid).unwrap_or(&0))
                .sum(),
            departed: before
                .switches
                .keys()
                .filter(|tid| !self.switches.contains_key(tid))
                .count() as u64,
        }
    }
}

pub(super) fn run(bencher: Bencher) {
    let rooms = scale_rooms(&[(50, 20), (200, 20), (400, 20), (200, 80)]);
    let compare = std::env::var("RIMZ_SCALE_COMPARE_SUPERVISOR").as_deref() == Ok("1");
    bencher.bench_local(|| {
        for (index, &(agents, sidebars)) in rooms.iter().enumerate() {
            let room = Room::new(agents, sidebars, HostStart::Prestarted).unwrap();
            room.warm();
            if compare && index == 0 {
                room.measure("parity_idle", WINDOW);
            }
            room.measure("idle", WINDOW);
            room.measure("busy", WINDOW);
            room.finish();
            if compare && index == 0 {
                let room = Room::new(agents, sidebars, HostStart::Supervisor).unwrap();
                room.warm();
                room.measure("parity_idle", WINDOW);
                room.finish();
            }
            match Room::new(agents, sidebars, HostStart::Traced) {
                Ok(room) => {
                    room.warm();
                    room.trace();
                    room.finish();
                }
                Err(reason) => emit(
                    KEY,
                    json!({"kind": "trace", "agents": agents, "sidebars": sidebars,
                    "skipped": reason}),
                ),
            }
        }
    });
}
