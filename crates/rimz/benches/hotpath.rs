#[cfg(not(rimz_jemalloc))]
use std::alloc::System as InnerAllocator;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;

use divan::Bencher;
use tempfile::TempDir;
#[cfg(rimz_jemalloc)]
use tikv_jemallocator::Jemalloc as InnerAllocator;

#[global_allocator]
static ALLOC: divan::AllocProfiler<InnerAllocator> = divan::AllocProfiler::new(InnerAllocator);

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
/// Rotated history rows in the fold benches: the main room's carryover shape
/// (~1,500 rows of ~8 KB JSON each).
const HISTORY_CARRYOVER: usize = 1_500;
const CARRYOVER_PROMPT_BYTES: usize = 2_500;
const SPENDING_FILES: usize = 4;
const SPENDING_ENTRIES_PER_FILE: usize = 5_000;
const SPENDING_NOW_SECS: u64 = 1_780_394_400;

fn main() {
    divan::main();
}

struct HookFixture {
    _sandbox: rimz::testkit::sandbox::TestSandbox,
    home_root: TempDir,
    runtime_root: TempDir,
    binary: PathBuf,
}

impl HookFixture {
    fn new() -> Self {
        let home_root = tempfile::Builder::new()
            .prefix("rimz-test-home-")
            .tempdir()
            .unwrap();
        let runtime_root = tempfile::Builder::new()
            .prefix("rr")
            .tempdir_in("/tmp")
            .unwrap();
        let binary = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("rimz");
        let sandbox = rimz::testkit::sandbox::TestSandbox::arm(
            rimz::testkit::sandbox::SandboxSpec {
                home_root: home_root.path().into(),
                runtime_root: runtime_root.path().into(),
            },
            &binary.with_file_name("rimz-test-reaper"),
        )
        .unwrap();
        Self {
            _sandbox: sandbox,
            home_root,
            runtime_root,
            binary,
        }
    }
}

#[cfg(target_os = "linux")]
#[divan::bench(sample_count = 1, sample_size = 1)]
fn drainer_latency(bencher: Bencher) {
    latency::run(bencher);
}

#[cfg(target_os = "linux")]
#[divan::bench(sample_count = 1, sample_size = 1, ignore)]
fn drainer_backlog(bencher: Bencher) {
    latency::run_backlog(bencher);
}

#[cfg(target_os = "linux")]
mod latency {
    use super::*;
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use rimz::agents::{AgentContext, AgentLifecycleObservation, LifecycleSignal};
    use rimz::disk::lock::SharedAppendLock;
    use rimz::harness::hook_drain::{Connection, DrainReceipt, HookDrainErr};
    use rimz::ids::{AgentKind, EventId};
    use rimz::store::event::{EventEnvelope, EventKind};
    use rimz::store::{agent_context, event_log, ingress};
    use serde_json::{Value, json};

    const SAMPLES: usize = 300;
    const WAIT: Duration = Duration::from_secs(120);

    struct Fixture {
        owners: SessionOwners,
        _roots: HookFixture,
        binary: PathBuf,
        project: PathBuf,
        env: BTreeMap<String, String>,
        store: rimz::Store,
    }

    struct SessionOwners(Vec<Child>);

    impl Drop for SessionOwners {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl Fixture {
        fn new(row_key: &str) -> Self {
            assert!(
                std::env::var_os("RIMZ_HOOK_BUDGET_HOME").is_none(),
                "generated fixture only"
            );
            let roots = HookFixture::new();
            let binary = std::env::var_os("RIMZ_HOOK_BUDGET_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| roots.binary.clone());
            assert!(
                binary.is_absolute() && binary.is_file(),
                "absolute CLI binary required"
            );
            let binary = binary.canonicalize().unwrap();
            let root = roots.home_root.path();
            let home = root.join("state");
            let project = root.join("repo");
            std::fs::create_dir(&project).unwrap();
            let paths = rimz::StatePaths::for_project_root_under(&project, &home).unwrap();
            let runtime = rimz::RuntimePaths::for_state_under(&paths, roots.runtime_root.path());
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
                ("XDG_RUNTIME_DIR", roots.runtime_root.path().into()),
            ] {
                std::fs::create_dir_all(&path).unwrap();
                env.insert(key.to_owned(), path.to_str().unwrap().to_owned());
            }
            env.extend([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("SHELL".into(), "/bin/sh".into()),
                ("RIMZ_WORKSPACE_ID".into(), paths.workspace_id.to_string()),
                ("RIMZ_PROJECT_ROOT".into(), project.to_str().unwrap().into()),
                ("RIMZ_BIN".into(), binary.to_str().unwrap().into()),
                ("RIMZ_RUNTIME_ENV".into(), "1".into()),
                ("RIMZ_TEST_HOOK_DRAIN_TIMING".into(), "1".into()),
            ]);
            // `systemd-run --user` reaches the manager only through
            // `$XDG_RUNTIME_DIR/systemd`, so the private runtime root links the caller's.
            if let Some(manager) = std::env::var_os("XDG_RUNTIME_DIR")
                .map(|dir| PathBuf::from(dir).join("systemd"))
                .filter(|dir| dir.is_dir())
            {
                std::os::unix::fs::symlink(manager, roots.runtime_root.path().join("systemd"))
                    .unwrap();
            }
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
                let output = Command::new("git")
                    .env_clear()
                    .envs(&env)
                    .current_dir(&project)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
            }
            let store = rimz::Store::open(paths, runtime).unwrap();
            let resolved =
                rimz::WorkspaceResolver::resolve_under(&project, Some(project.clone()), &home)
                    .unwrap();
            store.record_workspace(&resolved).unwrap();
            rimz::testkit::fleet::seed_history_carryover(
                &store,
                HISTORY_CARRYOVER,
                CARRYOVER_PROMPT_BYTES,
            )
            .unwrap();
            for index in 0..HISTORY_CARRYOVER {
                let id = format!("history-{index}");
                let observation =
                    AgentLifecycleObservation::new(Some(id.clone().into()), LifecycleSignal::Ended);
                event_log::append(
                    &store.paths().events_log,
                    &EventEnvelope::agent_lifecycle(
                        store.paths().workspace_id.clone(),
                        id,
                        "claude",
                        "SessionEnd",
                        &observation,
                    ),
                )
                .unwrap();
            }
            let fixture = Self {
                owners: SessionOwners(Vec::new()),
                _roots: roots,
                binary,
                project,
                env,
                store,
            };
            let mut fixture = fixture;
            for index in 0..SAMPLES {
                let owner = Command::new("sleep")
                    .arg("86400")
                    .env_clear()
                    .envs(&fixture.env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                fixture.owners.0.push(owner);
                let session = format!("latency-{index}");
                let frame = fixture.frame(index, json!({
                    "hook_event_name": "SessionStart", "session_id": session, "source": "startup",
                }));
                let output = output(
                    fixture.command(index).args(["hooks", "apply"]),
                    &serde_json::to_vec(&frame).unwrap(),
                );
                assert!(output.status.success(), "registration {index}: {output:?}");
                agent_context::write(
                    fixture.store.runtime_paths(),
                    "claude",
                    &session,
                    &AgentContext::new("claude", jiff::Timestamp::now()),
                    None,
                )
                .unwrap();
            }
            let snapshot = fixture.store.snapshot_cached().unwrap();
            let live: Vec<_> = snapshot
                .agents
                .iter()
                .filter(|agent| agent.ended_at.is_none())
                .collect();
            assert_eq!(live.len(), SAMPLES);
            for index in 0..SAMPLES {
                let id = format!("latency-{index}");
                assert!(live.iter().any(|agent| {
                    agent.agent_id.as_str() == id && !agent.is_provider_subagent()
                }));
            }
            let rollup = bytes(&fixture.store.paths().rollup_cache);
            assert!(
                rollup >= 1024 * 1024,
                "retained MB-scale rollup required: {rollup}"
            );
            let sidecars: Vec<_> =
                std::fs::read_dir(&fixture.store.runtime_paths().agent_context_dir)
                    .unwrap()
                    .map(|entry| entry.unwrap().metadata().unwrap().len())
                    .collect();
            assert_eq!(sidecars.len(), SAMPLES);
            let version = output(fixture.command(0).arg("--version"), &[]);
            assert!(version.status.success(), "{version:?}");
            emit(
                row_key,
                json!({"kind": "fixture", "binary": fixture.binary,
                "build_id": rimz::build_id::of_file(&fixture.binary).unwrap(),
                "version": String::from_utf8(version.stdout).unwrap().trim(),
                "live_roots": live.len(), "owner_processes": fixture.owners.0.len(),
                "history_rows": HISTORY_CARRYOVER, "rollup_bytes": rollup,
                "carryover_bytes": bytes(&fixture.store.paths().agents_carryover),
                "sidecar_count": sidecars.len(), "sidecar_bytes": sidecars.iter().sum::<u64>()}),
            );
            assert!(
                !fixture
                    .store
                    .runtime_paths()
                    .hook_drainer_socket_path()
                    .exists()
            );
            fixture
        }

        fn command(&self, index: usize) -> Command {
            let mut command = Command::new(&self.binary);
            command
                .env_clear()
                .envs(&self.env)
                .env("RIMZ_AGENT_PID", self.owners.0[index].id().to_string())
                .env("TMUX_PANE", format!("%{}", index + 1))
                .current_dir(&self.project)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command
        }

        fn frame(&self, index: usize, mut payload: Value) -> ingress::HookIngress {
            payload["cwd"] = json!(self.project);
            let mut env = self.env.clone();
            env.insert(
                "RIMZ_AGENT_PID".into(),
                self.owners.0[index].id().to_string(),
            );
            // The view reaps older paneless roots in the same worktree, even with distinct pids.
            env.insert("TMUX_PANE".into(), format!("%{}", index + 1));
            ingress::HookIngress {
                boot_id: rimz::proc::boot_id(),
                schema_version: "1".into(),
                event_id: EventId::new(),
                ts: jiff::Timestamp::now(),
                source: AgentKind::new_unchecked("claude"),
                event: Some(payload["hook_event_name"].as_str().unwrap().into()),
                payload: payload.to_string(),
                cwd: self.project.clone(),
                hook_pid: std::process::id(),
                env,
            }
        }

        fn append_receipt(&self, frame: &ingress::HookIngress) -> StopSample {
            let start = Instant::now();
            let lock = SharedAppendLock::acquire(&self.store.paths().hook_ingress_lock).unwrap();
            let through = ingress::append(self.store.paths(), frame, &lock).unwrap();
            drop(lock);
            let receipt = Connection::connect(&self.store)
                .unwrap()
                .reply(through, frame.event_id.clone(), WAIT)
                .unwrap();
            let wall = micros(start.elapsed());
            assert!(
                receipt.applied >= through && receipt.epoch.is_some(),
                "{receipt:?} does not cover {through}"
            );
            StopSample {
                ingress: frame.event_id.clone(),
                through,
                receipt,
                wall,
            }
        }
    }

    struct StopSample {
        ingress: EventId,
        through: u64,
        receipt: DrainReceipt,
        wall: u64,
    }

    fn output(command: &mut Command, payload: &[u8]) -> Output {
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(payload).unwrap();
        child.wait_with_output().unwrap()
    }

    fn bytes(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len()
    }

    fn micros(duration: Duration) -> u64 {
        duration.as_micros().try_into().unwrap()
    }

    fn emit(row_key: &str, row: Value) {
        // Divan can leave its tree label unterminated before fixture setup.
        writeln!(io::stdout().lock(), "\n{}", json!({row_key: row})).unwrap();
    }

    fn distribution(samples: &[u64]) -> Value {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        json!({"p50": sorted[(sorted.len() * 50).div_ceil(100) - 1],
            "p99": sorted[(sorted.len() * 99).div_ceil(100) - 1], "max": sorted.last().unwrap()})
    }

    fn timings(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|record| record.get("hook_drain_timing").cloned())
            .collect()
    }

    #[derive(serde::Serialize)]
    struct Placement {
        pid: u32,
        cgroup: String,
        cpu_weight: Option<u64>,
    }

    fn placement(pid: u32) -> Option<Placement> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
        let cgroup = text
            .lines()
            .find_map(|line| line.strip_prefix("0::"))?
            .to_owned();
        let weight = Path::new("/sys/fs/cgroup")
            .join(cgroup.trim_start_matches('/'))
            .join("cpu.weight");
        Some(Placement {
            pid,
            cgroup,
            cpu_weight: std::fs::read_to_string(weight)
                .ok()
                .and_then(|text| text.trim().parse().ok()),
        })
    }

    struct ChildSampler {
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<BTreeMap<u32, Placement>>>,
    }

    impl ChildSampler {
        fn start(pid: u32) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let done = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                let mut seen = BTreeMap::new();
                while !done.load(Ordering::Relaxed) {
                    let children =
                        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                            .unwrap_or_default();
                    for child in children
                        .split_whitespace()
                        .filter_map(|id| id.parse::<u32>().ok())
                    {
                        if seen.contains_key(&child) {
                            continue;
                        }
                        let cmdline =
                            std::fs::read(format!("/proc/{child}/cmdline")).unwrap_or_default();
                        let argv: Vec<_> = cmdline.split(|byte| *byte == 0).collect();
                        if argv
                            .windows(2)
                            .any(|pair| pair == [b"hooks".as_slice(), b"apply".as_slice()])
                            && let Some(place) = placement(child)
                        {
                            seen.insert(child, place);
                        }
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                seen
            });
            Self {
                stop,
                worker: Some(worker),
            }
        }

        fn finish(mut self) -> BTreeMap<u32, Placement> {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap()
        }
    }

    impl Drop for ChildSampler {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn start(fixture: &Fixture, row_key: &str) -> (UnixStream, Placement, ChildSampler, PathBuf) {
        emit(
            row_key,
            json!({
                "kind": "runner_placement", "placement": placement(std::process::id()).unwrap(),
            }),
        );
        let cold_start = Instant::now();
        let cold = json!({
            "hook_event_name": "SessionStart", "session_id": "latency-0",
            "source": "startup", "cwd": fixture.project,
        });
        let result = output(
            fixture
                .command(0)
                .args(["hooks", "feed", "--source", "claude"]),
            &serde_json::to_vec(&cold).unwrap(),
        );
        let foreground = micros(cold_start.elapsed());
        assert!(result.status.success(), "{result:?}");
        let deadline = Instant::now() + WAIT;
        let mut lease = loop {
            match UnixStream::connect(fixture.store.runtime_paths().hook_drainer_socket_path()) {
                Ok(stream) => break stream,
                Err(error) => {
                    assert!(Instant::now() < deadline, "drainer readiness: {error}");
                    thread::sleep(Duration::from_millis(5));
                }
            }
        };
        lease.set_read_timeout(Some(WAIT)).unwrap();
        lease.set_write_timeout(Some(WAIT)).unwrap();
        writeln!(
            lease,
            "{}",
            json!({
                "through": 0, "reply_for": null, "epoch": null, "stop": false, "lease": true,
            })
        )
        .unwrap();
        let mut line = String::new();
        io::BufReader::new(&lease).read_line(&mut line).unwrap();
        let _: DrainReceipt = serde_json::from_str(&line).unwrap();
        let pid = nix::sys::socket::getsockopt(&lease, nix::sys::socket::sockopt::PeerCredentials)
            .unwrap()
            .pid()
            .try_into()
            .unwrap();
        let drainer = placement(pid).unwrap();
        let sampler = ChildSampler::start(pid);
        let log = fixture.store.paths().hook_drainer_log();
        loop {
            if timings(&log)
                .iter()
                .any(|record| record["hook_event"] == "SessionStart" && record["applied"] == true)
            {
                break;
            }
            assert!(Instant::now() < deadline, "testkit C7 cold timing absent");
            thread::sleep(Duration::from_millis(5));
        }
        emit(
            row_key,
            json!({
                "kind": "cold", "foreground_wall_us": foreground,
                "ready_wall_us": micros(cold_start.elapsed()), "drainer": drainer,
            }),
        );
        (lease, drainer, sampler, log)
    }

    pub(super) fn run(bencher: Bencher) {
        let row_key = "drainer_latency";
        let fixture = Fixture::new(row_key);
        bencher.bench_local(|| {
            let (lease, drainer, sampler, log) = start(&fixture, row_key);
            let cold = json!({
                "hook_event_name": "SessionStart", "session_id": "latency-0",
                "source": "startup", "cwd": fixture.project,
            });
            let mut stops = Vec::new();
            for index in 0..SAMPLES {
                let frame = fixture.frame(
                    index,
                    json!({
                        "hook_event_name": "Stop", "session_id": format!("latency-{index}"),
                        "turn_id": format!("stop-{index}"), "last_assistant_message": "done",
                    }),
                );
                stops.push(fixture.append_receipt(&frame));
            }
            let mut prompt_walls = Vec::new();
            let mut misses = 0;
            for index in 0..SAMPLES {
                let session = format!("latency-{index}");
                let payload = serde_json::to_vec(&json!({
                    "hook_event_name": "UserPromptSubmit", "session_id": session,
                    "prompt": "check the repository", "cwd": fixture.project,
                }))
                .unwrap();
                let start = Instant::now();
                let result = output(
                    fixture
                        .command(index)
                        .args(["hooks", "feed", "--source", "claude"]),
                    &payload,
                );
                let wall = micros(start.elapsed());
                let reply = serde_json::from_slice::<Value>(&result.stdout).ok();
                let replied = result.status.success()
                    && reply.as_ref().is_some_and(|reply| {
                        reply["hookSpecificOutput"]["hookEventName"] == "UserPromptSubmit"
                            && reply["hookSpecificOutput"]["additionalContext"]
                                .as_str()
                                .is_some_and(|text| !text.trim().is_empty())
                    });
                misses += usize::from(!replied);
                prompt_walls.push(wall);
                emit(
                    row_key,
                    json!({
                        "kind": "prompt", "session": session, "wall_us": wall, "replied": replied,
                        "status": result.status.code(), "stdout_bytes": result.stdout.len(),
                    }),
                );
                assert!(result.status.success(), "prompt {index}: {result:?}");
            }
            let fence = fixture.frame(0, cold);
            fixture.append_receipt(&fence);
            let mut records = HashMap::new();
            for record in timings(&log) {
                let id = record["ingress"].as_str().unwrap().to_owned();
                assert!(records.insert(id, record).is_none(), "duplicate C7 ingress");
            }
            let events = event_log::read_all(&fixture.store.paths().events_log).unwrap();
            let mut claims = Vec::new();
            let mut receipts = Vec::new();
            for sample in stops {
                let record = records
                    .get(&sample.ingress.to_string())
                    .expect("missing measured C7 ingress");
                assert_eq!(record["hook_event"], "Stop");
                assert_eq!(record["applied"], true);
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| {
                            event.ingress.as_ref() == Some(&sample.ingress)
                                && matches!(event.kind(), EventKind::AgentLifecycle(_))
                        })
                        .count(),
                    1
                );
                let claim = record["claim_to_applied_us"].as_u64().unwrap();
                claims.push(claim);
                receipts.push(sample.wall);
                emit(
                    row_key,
                    json!({
                        "kind": "stop", "ingress": sample.ingress, "through": sample.through,
                        "receipt": sample.receipt, "claim_to_applied_us": claim,
                        "append_to_receipt_us": sample.wall,
                    }),
                );
            }
            let children = sampler.finish();
            assert!(!children.is_empty(), "no apply placement sampled");
            for child in children.values() {
                emit(
                    row_key,
                    json!({"kind": "apply_placement", "placement": child}),
                );
                assert_eq!(
                    child.cgroup, drainer.cgroup,
                    "apply must inherit the drainer"
                );
            }
            emit(
                row_key,
                json!({
                    "kind": "summary", "stop_samples": claims.len(),
                    "prompt_samples": prompt_walls.len(), "prompt_reply_misses": misses,
                    "claim_to_applied_us": distribution(&claims),
                    "append_to_receipt_us": distribution(&receipts),
                    "prompt_wall_us": distribution(&prompt_walls),
                    "apply_children_sampled": children.len(),
                }),
            );
            drop(lease);
        });
    }

    const BACKLOG_KEY: &str = "drainer_backlog";
    const RATES: &[u64] = &[1, 2, 4, 8, 16];
    const STEP: Duration = Duration::from_secs(60);
    const SENDERS: usize = 46;
    const WINDOW: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(10);
    const PROMPT_WAIT: Duration = Duration::from_millis(1_200);
    const DRAIN_WAIT: Duration = Duration::from_secs(600);

    type TimedRecord = (Instant, Value);

    struct TimingReader {
        stop: Arc<AtomicBool>,
        records: Arc<Mutex<Vec<TimedRecord>>>,
        worker: Option<JoinHandle<()>>,
    }

    impl TimingReader {
        fn start(path: &Path) -> Self {
            let mut reader = io::BufReader::new(std::fs::File::open(path).unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let records = Arc::new(Mutex::new(Vec::new()));
            let done = Arc::clone(&stop);
            let pending = Arc::clone(&records);
            let worker = thread::spawn(move || {
                let mut line = String::new();
                loop {
                    while reader.read_line(&mut line).unwrap() != 0 {
                        if !line.ends_with('\n') {
                            continue;
                        }
                        // Publish the read stamp under the snapshot lock, so a window cannot
                        // miss an already-stamped record at its boundary.
                        let mut records = pending.lock().unwrap();
                        let read_at = Instant::now();
                        if let Ok(mut record) = serde_json::from_str::<Value>(&line)
                            && let Some(timing) = record.get_mut("hook_drain_timing")
                        {
                            records.push((read_at, timing.take()));
                        }
                        line.clear();
                    }
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(POLL);
                }
            });
            Self {
                stop,
                records,
                worker: Some(worker),
            }
        }

        fn finish(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    impl Drop for TimingReader {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    struct BacklogFrame {
        hook: &'static str,
        appended: Instant,
        sent_at: Instant,
        send_lag: u64,
        applied: Option<(Instant, u64)>,
    }

    #[derive(Default)]
    struct Sweep {
        frames: Vec<BacklogFrame>,
        indices: HashMap<EventId, usize>,
    }

    impl Sweep {
        fn send(&mut self, fixture: &Fixture, due: Instant) -> Option<JoinHandle<Option<u64>>> {
            let sequence = self.frames.len();
            let sender = sequence % SENDERS;
            let turn_offset = (sequence / SENDERS + sender) % 20;
            let (hook, mut payload) = match turn_offset {
                0 => (
                    "UserPromptSubmit",
                    json!({"prompt": "check the repository"}),
                ),
                19 => (
                    "Stop",
                    json!({
                        "turn_id": format!("backlog-{sequence}"),
                        "last_assistant_message": "done",
                    }),
                ),
                _ => (
                    "PostToolUse",
                    json!({
                        "tool_name": "Read", "tool_input": {}, "tool_response": {},
                    }),
                ),
            };
            payload["hook_event_name"] = json!(hook);
            payload["session_id"] = json!(format!("latency-{sender}"));
            let frame = fixture.frame(sender, payload);
            let appended = Instant::now();
            let lock = SharedAppendLock::acquire(&fixture.store.paths().hook_ingress_lock).unwrap();
            let through = ingress::append(fixture.store.paths(), &frame, &lock).unwrap();
            let sent_at = Instant::now();
            drop(lock);
            self.indices.insert(frame.event_id.clone(), sequence);
            self.frames.push(BacklogFrame {
                hook,
                appended,
                sent_at,
                send_lag: micros(sent_at.duration_since(due)),
                applied: None,
            });
            let connection = Connection::connect(&fixture.store).unwrap();
            if hook == "UserPromptSubmit" {
                return Some(thread::spawn(move || {
                    prompt_receipt(connection, through, frame.event_id, appended)
                }));
            }
            connection.nudge(through).unwrap();
            None
        }

        fn observe(&mut self, reader: &TimingReader, mut lease: &UnixStream) -> Instant {
            assert!(
                matches!(lease.read(&mut [0]), Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock),
                "drainer lease closed during sweep"
            );
            let mut records = reader.records.lock().unwrap();
            let now = Instant::now();
            for (read_at, record) in records.drain(..) {
                let id: EventId = serde_json::from_value(record["ingress"].clone()).unwrap();
                let Some(&index) = self.indices.get(&id) else {
                    continue;
                };
                let frame = &mut self.frames[index];
                assert_eq!(record["hook_event"], frame.hook, "{id}");
                assert_eq!(record["applied"], true, "{id}");
                let claim = record["claim_to_applied_us"].as_u64().unwrap();
                assert!(
                    frame.applied.replace((read_at, claim)).is_none(),
                    "duplicate sweep ingress {id}"
                );
            }
            now
        }

        fn drain(
            &mut self,
            reader: &TimingReader,
            lease: &UnixStream,
            first: usize,
            send_end: Instant,
            mut window: BacklogWindow,
        ) -> Instant {
            loop {
                let now = self.observe(reader, lease);
                let depth = self
                    .frames
                    .iter()
                    .filter(|frame| frame.applied.is_none())
                    .count();
                assert!(
                    now < send_end + DRAIN_WAIT,
                    "step {} drain timeout: depth {depth}",
                    window.step,
                );
                if depth == 0 {
                    let end = self.frames[first..]
                        .iter()
                        .map(|frame| frame.applied.unwrap().0)
                        .max()
                        .unwrap()
                        .max(send_end);
                    window.report(&self.frames, end, true);
                    return end;
                }
                window.report(&self.frames, now, false);
                thread::sleep(POLL);
            }
        }
    }

    struct BacklogWindow {
        sweep_start: Instant,
        start: Instant,
        step: usize,
        rate: u64,
        phase: &'static str,
    }

    impl BacklogWindow {
        fn report(&mut self, frames: &[BacklogFrame], now: Instant, finish: bool) {
            while self.start + WINDOW <= now || finish {
                let end = (self.start + WINDOW).min(now);
                let sent = frames.iter().filter(|frame| frame.sent_at <= end).count();
                let applied = frames
                    .iter()
                    .filter(|frame| frame.applied.is_some_and(|(at, _)| at <= end))
                    .count();
                let mut claims = Vec::new();
                let mut appends = Vec::new();
                for frame in frames {
                    if let Some((at, claim)) = frame.applied
                        && at > self.start
                        && at <= end
                    {
                        claims.push(claim);
                        appends.push(micros(at.duration_since(frame.appended)));
                    }
                }
                let loadavg: f64 = std::fs::read_to_string("/proc/loadavg")
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                emit(
                    BACKLOG_KEY,
                    json!({
                        "kind": "window", "t_ms": end.duration_since(self.sweep_start).as_millis(),
                        "step": self.step, "rate_per_s": self.rate, "phase": self.phase,
                        "sent": sent, "applied": applied, "depth": sent - applied,
                        "applied_in_window": claims.len(), "loadavg_1m": loadavg,
                        "claim_to_applied_us": nullable_distribution(&claims),
                        "append_to_applied_us": nullable_distribution(&appends),
                    }),
                );
                self.start = end;
                if end == now {
                    break;
                }
            }
        }
    }

    fn nullable_distribution(samples: &[u64]) -> Value {
        if samples.is_empty() {
            Value::Null
        } else {
            distribution(samples)
        }
    }

    fn prompt_receipt(
        connection: Connection,
        through: u64,
        id: EventId,
        appended: Instant,
    ) -> Option<u64> {
        let start = Instant::now();
        match connection.reply(through, id, PROMPT_WAIT) {
            Ok(receipt) => {
                assert!(
                    receipt.applied >= through && receipt.epoch.is_some(),
                    "{receipt:?}"
                );
                (start.elapsed() <= PROMPT_WAIT).then(|| micros(appended.elapsed()))
            }
            Err(HookDrainErr::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            Err(error) => panic!("prompt reply: {error}"),
        }
    }

    fn report_step(
        step: usize,
        rate: u64,
        frames: &[BacklogFrame],
        send_end: Instant,
        send_duration: Duration,
        drain_duration: Duration,
        prompts: Vec<JoinHandle<Option<u64>>>,
    ) {
        let mut claims = Vec::new();
        let mut appends = Vec::new();
        let mut by_hook = BTreeMap::<_, Vec<_>>::new();
        let mut depth_changes = Vec::new();
        let mut applied_by_end = 0;
        for frame in frames {
            let (at, claim) = frame.applied.unwrap();
            claims.push(claim);
            appends.push(micros(at.duration_since(frame.appended)));
            by_hook.entry(frame.hook).or_default().push(claim);
            applied_by_end += usize::from(at <= send_end);
            depth_changes.extend([(frame.sent_at, 1_i64), (at, -1)]);
        }
        depth_changes.sort_unstable();
        let mut depth = 0;
        let mut depth_max = 0;
        for (_, change) in depth_changes {
            depth += change;
            depth_max = depth_max.max(depth);
        }
        let prompt_count = prompts.len();
        let receipts: Vec<_> = prompts
            .into_iter()
            .filter_map(|prompt| prompt.join().unwrap())
            .collect();
        let by_hook: BTreeMap<_, _> = by_hook
            .into_iter()
            .map(|(hook, samples)| (hook, distribution(&samples)))
            .collect();
        let send_lags: Vec<_> = frames.iter().map(|frame| frame.send_lag).collect();
        emit(
            BACKLOG_KEY,
            json!({
                "kind": "step", "step": step, "rate_per_s": rate, "step_s": STEP.as_secs(),
                "sent": frames.len(), "send_lag_us": distribution(&send_lags),
                "applied_by_end": applied_by_end, "depth_end": frames.len() - applied_by_end,
                "applied_rate_per_s": applied_by_end as f64 / send_duration.as_secs_f64(),
                "depth_max": depth_max, "drain_ms": drain_duration.as_millis(),
                "append_to_applied_us": distribution(&appends),
                "claim_to_applied_us": distribution(&claims),
                "claim_to_applied_us_by_hook": by_hook,
                "prompts": prompt_count, "prompts_late": prompt_count - receipts.len(),
                "prompt_receipt_us": nullable_distribution(&receipts),
            }),
        );
    }

    fn run_sweep(fixture: &Fixture) {
        let (lease, _, sampler, log) = start(fixture, BACKLOG_KEY);
        drop(sampler);
        lease.set_nonblocking(true).unwrap();
        for index in 0..SENDERS {
            fixture.append_receipt(&fixture.frame(
                index,
                json!({
                    "hook_event_name": "UserPromptSubmit", "session_id": format!("latency-{index}"),
                    "prompt": "check the repository",
                }),
            ));
        }
        emit(
            BACKLOG_KEY,
            json!({
                "kind": "config", "rates_per_s": RATES, "step_s": STEP.as_secs(),
                "senders": SENDERS, "turn": {"UserPromptSubmit": 1, "PostToolUse": 18, "Stop": 1},
                "window_s": WINDOW.as_secs(), "poll_ms": POLL.as_millis(),
                "prompt_wait_ms": PROMPT_WAIT.as_millis(), "live_roots": SAMPLES,
            }),
        );
        let mut reader = TimingReader::start(&log);
        let mut sweep = Sweep::default();
        let sweep_start = Instant::now();
        for (index, &rate) in RATES.iter().enumerate() {
            let step = index + 1;
            let first = sweep.frames.len();
            let step_start = Instant::now();
            let scheduled_end = step_start + STEP;
            let count = (rate * STEP.as_secs()) as usize;
            let mut prompts = Vec::new();
            let mut window = BacklogWindow {
                sweep_start,
                start: step_start,
                step,
                rate,
                phase: "send",
            };
            let mut sent = 0;
            while sent < count || Instant::now() < scheduled_end {
                let now = sweep.observe(&reader, &lease);
                let window_end = if sent == count {
                    now.min(scheduled_end.max(sweep.frames.last().unwrap().sent_at))
                } else {
                    now
                };
                window.report(&sweep.frames, window_end, false);
                let now = Instant::now();
                let due = step_start + Duration::from_secs_f64(sent as f64 / rate as f64);
                if sent < count && now >= due {
                    prompts.extend(sweep.send(fixture, due));
                    sent += 1;
                    continue;
                }
                thread::sleep(POLL.min(due.saturating_duration_since(Instant::now())));
            }
            sweep.observe(&reader, &lease);
            let send_end = scheduled_end.max(sweep.frames.last().unwrap().sent_at);
            window.report(&sweep.frames, send_end, true);
            let window = BacklogWindow {
                sweep_start,
                start: send_end,
                step,
                rate,
                phase: "drain",
            };
            let drain_end = sweep.drain(&reader, &lease, first, send_end, window);
            assert_eq!(sweep.frames.len() - first, count);
            report_step(
                step,
                rate,
                &sweep.frames[first..],
                send_end,
                send_end.duration_since(step_start),
                drain_end.duration_since(send_end),
                prompts,
            );
        }
        reader.finish();
        sweep.observe(&reader, &lease);
        assert!(sweep.frames.iter().all(|frame| frame.applied.is_some()));
        drop(lease);
    }

    pub(super) fn run_backlog(bencher: Bencher) {
        let fixture = Fixture::new(BACKLOG_KEY);
        bencher.bench_local(|| run_sweep(&fixture));
    }
}

#[divan::bench(sample_count = 1, sample_size = 1)]
#[expect(
    clippy::print_stdout,
    reason = "benchmark reports paired sequential and burst wall budgets"
)]
fn hook_stop_budget(bencher: Bencher) {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    fn sample(mut command: Command, payload: &[u8], hook: bool) -> Duration {
        let start = Instant::now();
        let mut child = command.spawn().unwrap();
        let written = child.stdin.take().unwrap().write_all(payload);
        assert!(
            written.is_ok() || (!hook && written.unwrap_err().kind() == io::ErrorKind::BrokenPipe)
        );
        let status = child.wait().unwrap();
        let wall = start.elapsed();
        // These commands have bounded output; collection is outside timing,
        // without extra reader threads.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        assert!(status.success(), "{status:?}; stderr {stderr:?}");
        if hook {
            assert!(stdout.is_empty(), "{stdout:?}");
        }
        wall
    }

    /// Nearest-rank percentile: at `pct = 50` on an even count, the upper median.
    fn percentile(mut samples: Vec<Duration>, pct: usize) -> Duration {
        samples.sort_unstable();
        samples[(samples.len() * pct).div_ceil(100) - 1]
    }

    let fixture = HookFixture::new();
    let root = fixture.home_root.path();
    let binary = fixture.binary.clone();
    assert!(
        binary.is_file(),
        "build rimz in the bench profile before measuring"
    );
    let copied_home = std::env::var_os("RIMZ_HOOK_BUDGET_HOME");
    let home = root.join(if copied_home.is_some() {
        ".rimz"
    } else {
        "state"
    });
    let runtime = fixture.runtime_root.path();
    let (project_root, paths) = if let Some(source) = copied_home.as_ref() {
        assert!(
            Command::new("cp")
                .arg("-a")
                .arg("--")
                .arg(PathBuf::from(source).join(".rimz"))
                .arg(&home)
                .status()
                .unwrap()
                .success()
        );
        let mut rooms = rimz::workspace::known_workspaces_under(
            &rimz::disk::paths::workspaces_dir_under(&home),
        )
        .unwrap();
        assert_eq!(rooms.len(), 1, "copied home must contain exactly one room");
        let room = rooms.remove(0);
        let paths = rimz::StatePaths::under_named(room.workspace_id, room.dir_name, &home);
        for queue in [&paths.hook_ingress_log, &paths.hook_drain_cursor] {
            if queue.exists() {
                std::fs::remove_file(queue).unwrap();
            }
        }
        (room.project_root, paths)
    } else {
        (
            root.to_path_buf(),
            rimz::StatePaths::for_project_root_under(root, &home).unwrap(),
        )
    };
    let runtime_paths = rimz::RuntimePaths::for_state_under(&paths, runtime);
    let store = rimz::Store::open(paths.clone(), runtime_paths).unwrap();
    if copied_home.is_none() {
        let resolved =
            rimz::WorkspaceResolver::resolve_under(root, Some(root.into()), &home).unwrap();
        store.record_workspace(&resolved).unwrap();
        rimz::testkit::fleet::seed_fleet_store(&paths, FLEET, HISTORY_EVENTS).unwrap();
    }
    store.snapshot().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop", "session_id": "bench-stop", "turn_id": "bench-turn",
        "cwd": project_root, "last_assistant_message": "done"
    })
    .to_string();
    let command = || {
        let mut command = Command::new(&binary);
        command
            .env_clear()
            .env("HOME", root)
            .env("RIMZ_HOME", &home)
            .env("XDG_RUNTIME_DIR", runtime)
            .env("PATH", "/usr/bin:/bin")
            .env("RIMZ_WORKSPACE_ID", paths.workspace_id.as_str())
            .env("RIMZ_PROJECT_ROOT", &project_root)
            .env("RIMZ_AGENT_PID", std::process::id().to_string())
            .current_dir(&project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };
    let burst = |hook| {
        let barrier = Barrier::new(24);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..24)
                .map(|_| {
                    let barrier = &barrier;
                    let payload = payload.as_bytes();
                    let mut command = command();
                    if hook {
                        command.args(["hooks", "feed", "--source", "codex"]);
                    } else {
                        command.arg("--version");
                    }
                    scope.spawn(move || {
                        barrier.wait();
                        sample(command, payload, hook)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        })
    };
    let mut worker = command();
    let mut drainer = worker
        .args(["hooks", "drain", "--project-root"])
        .arg(&project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let deadline = Instant::now() + Duration::from_secs(5);
    let _lease = loop {
        match UnixStream::connect(&socket) {
            Ok(lease) => break lease,
            Err(error) => {
                assert!(Instant::now() < deadline, "drainer did not listen: {error}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let mut warm = command();
    warm.args(["hooks", "feed", "--source", "codex"]);
    sample(warm, payload.as_bytes(), true);
    assert!(
        command()
            .args(["hooks", "drain", "--project-root"])
            .arg(&project_root)
            .arg("--once")
            .output()
            .unwrap()
            .status
            .success()
    );
    for hook in [true, false] {
        drop(burst(hook));
    }
    let room = if copied_home.is_some() {
        "live-copy"
    } else {
        "seeded"
    };
    bencher.bench_local(|| {
        let mut hooks = Vec::new();
        let mut versions = Vec::new();
        for pair in 0usize..100 {
            for hook_first in [true, false] {
                let hook = pair.is_multiple_of(2) == hook_first;
                let mut command = command();
                if hook {
                    command.args(["hooks", "feed", "--source", "codex"]);
                } else {
                    command.arg("--version");
                }
                let measured = sample(command, payload.as_bytes(), hook);
                if hook {
                    hooks.push(measured);
                } else {
                    versions.push(measured);
                }
            }
        }
        // Single spawns stall at random, so the sequential guard holds the
        // median; tail contention is the 24-way half's to catch.
        let hook_max = percentile(hooks.clone(), 100);
        let version_max = percentile(versions.clone(), 100);
        let hook = percentile(hooks, 50);
        let version = percentile(versions, 50);
        let ceiling = Duration::from_millis(4).max(version * 2);
        println!(
            "{room} sequential: hook median {hook:?} max {hook_max:?}; --version median {version:?} max {version_max:?}; ceiling {ceiling:?}"
        );
        let mut hook_bursts = Vec::new();
        let mut version_bursts = Vec::new();
        for pair in 0..10 {
            for hook in [true, false] {
                let samples = burst(hook);
                println!(
                    "{room} burst {pair} {}: wall p99 {:?}",
                    if hook { "hook" } else { "version" },
                    percentile(samples.clone(), 99),
                );
                if hook {
                    hook_bursts.extend(samples);
                } else {
                    version_bursts.extend(samples);
                }
            }
        }
        let hook_burst = percentile(hook_bursts, 99);
        let version_burst = percentile(version_bursts, 99);
        let burst_ceiling = Duration::from_millis(4).max(version_burst * 2);
        println!("{room} 24-way: hook p99 {hook_burst:?}; --version p99 {version_burst:?}; ceiling {burst_ceiling:?}");
        assert!(
            hook <= ceiling,
            "hook median {hook:?} > {ceiling:?}; --version median {version:?}"
        );
        assert!(
            hook_burst <= burst_ceiling,
            "24-way hook p99 {hook_burst:?} > {burst_ceiling:?}; --version p99 {version_burst:?}"
        );
    });
    // Cleanup also includes the unmeasured warm-up; serial apply on the
    // live-sized copy can outlast the CLI's 30-second wait.
    let drain_start = Instant::now();
    rimz::harness::hook_drain::drain_through(&store, 0, None, Duration::from_secs(120)).unwrap();
    println!("{room} after-return drain: {:?}", drain_start.elapsed());
    drainer.kill().unwrap();
    drainer.wait().unwrap();
}

struct BenchWorkspace {
    _tempdir: TempDir,
    paths: rimz::StatePaths,
    runtime: rimz::RuntimePaths,
}

impl BenchWorkspace {
    fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let state_root = tempdir.path().join("state");
        let runtime_root = tempdir.path().join("runtime");
        let workspace_id = rimz::WorkspaceId::from_project_root(tempdir.path());
        let paths = rimz::StatePaths::under(workspace_id.clone(), &state_root).expect("paths");
        paths.ensure_dirs().expect("state dirs");
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("runtime");
        std::fs::create_dir_all(&runtime.root).expect("runtime root");
        std::fs::create_dir_all(&runtime.shared_root).expect("shared runtime root");
        Self {
            _tempdir: tempdir,
            paths,
            runtime,
        }
    }

    fn seed_fleet(&self, fleet: usize, history_events: usize) {
        rimz::testkit::fleet::seed_fleet_store(&self.paths, fleet, history_events)
            .expect("seed fleet");
    }

    fn publish_inputs(&self, fleet: usize) {
        rimz::testkit::fleet::publish_fresh_produce_inputs(&self.runtime, fleet)
            .expect("publish produce inputs");
    }
}

struct SnapshotFixture {
    _workspace: BenchWorkspace,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
}

struct FuseFixture {
    _workspace: BenchWorkspace,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
    events: rimz::sidebar::event_store::EventStore,
    now_ms: u64,
}

struct EnrichFixture {
    _workspace: BenchWorkspace,
    runtime: rimz::RuntimePaths,
    snapshot: rimz::store::snapshot::SidebarSnapshot,
    frame: rimz::sidebar::frame::PaneFrame,
}

struct FoldFixture {
    _workspace: BenchWorkspace,
    paths: rimz::StatePaths,
    cursor: rimz::sidebar::consumer::RollupCursor,
}

struct SpendingFixture {
    _tempdir: TempDir,
    cache_path: PathBuf,
    files: Vec<rimz::agents::spending::SpendingFile>,
    prices: rimz::agents::PriceBook,
    walker: rimz::agents::spending::SpendingWalker,
    sources: Vec<rimz::agents::spending::SpendingSource>,
}

struct ChangedSessionFixture {
    _tempdir: TempDir,
    refresh: rimz::testkit::ChangedSessionRefreshFixture,
}

fn changed_session_fixture(
    build: impl FnOnce(&std::path::Path, usize) -> rimz::testkit::ChangedSessionRefreshFixture,
) -> ChangedSessionFixture {
    let tempdir = TempDir::new().expect("tempdir");
    let refresh = build(tempdir.path(), 500);
    ChangedSessionFixture {
        _tempdir: tempdir,
        refresh,
    }
}

fn snapshot_fixture() -> SnapshotFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    workspace.publish_inputs(FLEET);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    let snapshot = rimz::sidebar::produce::produce_snapshot(
        &mut cursor,
        &workspace.paths,
        &workspace.runtime,
        &rimz::testkit::fleet::produce_options(),
        &mut rimz::sidebar::produce::SnapshotProducer::seeded(
            &workspace.runtime,
            rimz::testkit::fleet::SESSION_NAME,
        ),
    )
    .expect("produce snapshot");
    SnapshotFixture {
        _workspace: workspace,
        snapshot,
    }
}

fn owned_fuse_fixture() -> FuseFixture {
    let SnapshotFixture {
        _workspace,
        snapshot,
    } = snapshot_fixture();
    let now_ms = snapshot
        .panes_produced_at_ms
        .unwrap_or_else(rimz::utils::time::unix_now_ms)
        .saturating_add(1);
    FuseFixture {
        _workspace,
        snapshot,
        events: rimz::sidebar::event_store::EventStore::default(),
        now_ms,
    }
}

fn enrich_fixture() -> EnrichFixture {
    let workspace = BenchWorkspace::new();
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    workspace.publish_inputs(FLEET);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    let snapshot =
        rimz::sidebar::consumer::rollup_snapshot(&workspace.paths, &mut cursor).expect("rollup");
    let frame = rimz::sidebar::frame::assemble_frame(
        rimz::testkit::fleet::synthetic_panes(FLEET),
        rimz::utils::time::unix_now_ms(),
        rimz::testkit::fleet::SESSION_NAME,
    );
    EnrichFixture {
        runtime: workspace.runtime.clone(),
        _workspace: workspace,
        snapshot,
        frame,
    }
}

/// A warm cursor over `FLEET` live agents and `history_carryover` rotated
/// history rows (production-weight: each carries a `CARRYOVER_PROMPT_BYTES`
/// prompt), with one appended lifecycle frame when `append` is set.
fn fold_fixture(history_carryover: usize, append: bool) -> FoldFixture {
    let workspace = BenchWorkspace::new();
    if history_carryover > 0 {
        let store = rimz::Store::open(workspace.paths.clone(), workspace.runtime.clone())
            .expect("open store");
        rimz::testkit::fleet::seed_history_carryover(
            &store,
            history_carryover,
            CARRYOVER_PROMPT_BYTES,
        )
        .expect("stage carryover");
    }
    workspace.seed_fleet(FLEET, HISTORY_EVENTS);
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    cursor.fold(&workspace.paths).expect("cold fold");
    if append {
        rimz::store::event_log::append(
            &workspace.paths.events_log,
            &rimz::testkit::fleet::registered_lifecycle(&workspace.paths.workspace_id, 0),
        )
        .expect("append delta");
    }
    FoldFixture {
        paths: workspace.paths.clone(),
        cursor,
        _workspace: workspace,
    }
}

fn ended_carryover_fixture(rows: usize, rebirth: bool) -> FoldFixture {
    let workspace = BenchWorkspace::new();
    let store =
        rimz::Store::open(workspace.paths.clone(), workspace.runtime.clone()).expect("open store");
    rimz::testkit::fleet::seed_ended_carryover(&store, rows).expect("seed carryover");
    let export_dir = std::env::var_os("RIMZ_CARRYOVER_FIXTURE_DIR").map(PathBuf::from);
    if let Some(root) = &export_dir {
        std::fs::create_dir_all(root).expect("fixture export directory");
        let target = root.join(format!("carryover-{rows}.json"));
        if !target.exists() {
            std::fs::copy(&workspace.paths.agents_carryover, target).expect("export fixture");
        }
    }
    let mut cursor = rimz::sidebar::consumer::RollupCursor::new();
    if rebirth {
        rimz::store::event_log::append(
            &workspace.paths.events_log,
            &rimz::store::event::EventEnvelope::session_rebirth(
                workspace.paths.workspace_id.clone(),
                "bench-session",
            ),
        )
        .expect("append rebirth");
        if let Some(root) = &export_dir {
            std::fs::copy(
                &workspace.paths.events_log,
                root.join(format!("rebirth-{rows}.log.jsonl")),
            )
            .expect("export rebirth log");
        }
        cursor.fold(&workspace.paths).expect("prime reborn cursor");
        rimz::store::event_log::append(
            &workspace.paths.events_log,
            &rimz::testkit::fleet::registered_lifecycle(&workspace.paths.workspace_id, 0),
        )
        .expect("append registration");
    }
    FoldFixture {
        paths: workspace.paths.clone(),
        cursor,
        _workspace: workspace,
    }
}

fn spending_fixture(warm: bool) -> SpendingFixture {
    spending_fixture_scaled(SPENDING_FILES, SPENDING_ENTRIES_PER_FILE, warm, false)
}

fn spending_fixture_scaled(
    files_count: usize,
    entries_per_file: usize,
    warm: bool,
    scoped: bool,
) -> SpendingFixture {
    let tempdir = TempDir::new().expect("tempdir");
    let cache_path = tempdir.path().join("spending.json");
    let history_root = tempdir.path().join("history");
    let mut cache = rimz::agents::spending::read_spending_cache(&cache_path);
    cache.files = HashMap::new();
    let mut files = Vec::new();
    for file_index in 0..files_count {
        let transcript = history_root
            .join(format!("{:04}", file_index / 100))
            .join(format!("cached-{file_index}.jsonl"));
        std::fs::create_dir_all(transcript.parent().expect("history parent"))
            .expect("history directory");
        std::fs::write(&transcript, b"").expect("transcript");
        let entries = (0..entries_per_file)
            .map(|offset| {
                let index = file_index * entries_per_file + offset;
                rimz::agents::spending::CachedEntry {
                    ts_secs: SPENDING_NOW_SECS - 86_400
                        + u64::try_from(offset % 3_600).expect("offset fits u64"),
                    cost_usd: 0.001,
                    input: 1200,
                    output: 80,
                    cache_write: 0,
                    cache_read: 800,
                    tool_calls: Default::default(),
                    message_id: Some(format!("msg-{index}").into_boxed_str()),
                    request_id: Some(format!("req-{index}").into_boxed_str()),
                    dedup_key: None,
                    thread_id: Some(format!("thread-{index}").into_boxed_str()),
                    is_sidechain: false,
                    has_speed: false,
                    fast: false,
                    model: Some("claude-opus-4-8".into()),
                    rolled: false,
                    timing: None,
                }
            })
            .collect();
        cache.files.insert(
            transcript.to_string_lossy().into_owned(),
            rimz::agents::spending::FileCacheEntry {
                stat: rimz::agents::TranscriptStat::from_path(&transcript)
                    .expect("transcript stat"),
                cursor: rimz::agents::spending::SpendCursor::default(),
                origin_path: scoped.then(|| tempdir.path().to_path_buf()),
                entries,
                unknown_models: BTreeMap::new(),
            },
        );
        files.push(rimz::agents::spending::SpendingFile {
            adapter: rimz::agents::definition_by_kind("claude").expect("Claude definition"),
            login: rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
            path: transcript,
        });
    }
    rimz::agents::spending::write_spending_cache(&cache_path, &cache);
    let prices = rimz::agents::PriceBook::default();
    let sources = vec![rimz::agents::spending::SpendingSource::group(vec![
        rimz::agents::spending::SpendingSourceTree::new(&history_root, "**/*.jsonl")
            .expect("benchmark pattern"),
    ])];
    let mut walker = rimz::agents::spending::SpendingWalker::new();
    if warm {
        let origin_overrides = HashMap::new();
        let user_inputs = Vec::new();
        let spec = rimz::agents::spending::HeadlineSpec::default();
        let req = rimz::agents::spending::WalkRequest {
            files: &files,
            prices: &prices,
            now_secs: SPENDING_NOW_SECS,
            origin_overrides: &origin_overrides,
            user_inputs: &user_inputs,
            scope: None,
            spec: &spec,
        };
        let _ = walker.walk(&cache_path, &req, &mut rimz::agents::spending::SilentWalk);
    }
    SpendingFixture {
        _tempdir: tempdir,
        cache_path,
        files,
        prices,
        walker,
        sources,
    }
}

fn spending_discovery_fixture(warm: bool) -> SpendingFixture {
    let mut fixture = spending_fixture_scaled(6_000, 17, true, true);
    if warm {
        let _ = fixture.walker.discover_declared_spending_files(
            rimz::agents::definition_by_kind("claude").expect("Claude definition"),
            fixture.sources.clone(),
            SPENDING_NOW_SECS,
        );
    }
    fixture
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn fuse_owned_no_overlay(bencher: Bencher) {
    bencher
        .with_inputs(owned_fuse_fixture)
        .bench_local_values(|fixture| {
            divan::black_box(rimz::sidebar::fuse::fuse_owned(
                fixture.snapshot,
                fixture.events,
                None,
                fixture.now_ms,
            ));
        });
}

/// One appended frame folded onto a warm cursor, at an empty and at a
/// production-sized (`HISTORY_CARRYOVER`) rotation carryover. The fold benches
/// return the fixture so divan drops its tempdir and cursor outside the timing.
#[divan::bench(args = [0, HISTORY_CARRYOVER], sample_count = 20, sample_size = 1, skip_ext_time)]
fn rollup_fold_warm(bencher: Bencher, history_carryover: usize) {
    bencher
        .with_inputs(|| fold_fixture(history_carryover, true))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("warm fold"));
            fixture
        });
}

/// A warm cursor re-served with nothing appended — the idle wakeup.
#[divan::bench(args = [0, HISTORY_CARRYOVER], sample_count = 20, sample_size = 1, skip_ext_time)]
fn rollup_fold_unchanged(bencher: Bencher, history_carryover: usize) {
    bencher
        .with_inputs(|| fold_fixture(history_carryover, false))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("unchanged fold"));
            fixture
        });
}

#[divan::bench(args = [3_300, 13_200], sample_count = 20, sample_size = 1, skip_ext_time)]
fn carryover_fold_cold(bencher: Bencher, rows: usize) {
    bencher
        .with_inputs(|| ended_carryover_fixture(rows, false))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.cursor.fold(&fixture.paths).expect("cold fold"));
            fixture
        });
}

#[divan::bench(args = [3_300, 13_200], sample_count = 20, sample_size = 1, skip_ext_time)]
fn carryover_fold_rebirth_warm(bencher: Bencher, rows: usize) {
    bencher
        .with_inputs(|| ended_carryover_fixture(rows, true))
        .bench_local_values(|mut fixture| {
            divan::black_box(
                fixture
                    .cursor
                    .fold(&fixture.paths)
                    .expect("reborn delta fold"),
            );
            fixture
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_walk_cold(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture(false))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_walk_warm_no_change(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture(true))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_cold_hydrate(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture_scaled(6_000, 17, false, true))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_warm_global_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture_scaled(6_000, 17, true, true))
        .bench_local_values(|mut fixture| {
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &fixture.files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_cold_discovery_inclusive(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_discovery_fixture(false))
        .bench_local_values(|mut fixture| {
            let files = fixture.walker.discover_declared_spending_files(
                rimz::agents::definition_by_kind("claude").expect("Claude definition"),
                fixture.sources.clone(),
                SPENDING_NOW_SECS,
            );
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_warm_discovery_inclusive(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_discovery_fixture(true))
        .bench_local_values(|mut fixture| {
            let files = fixture.walker.discover_declared_spending_files(
                rimz::agents::definition_by_kind("claude").expect("Claude definition"),
                fixture.sources.clone(),
                SPENDING_NOW_SECS,
            );
            let origin_overrides = HashMap::new();
            let user_inputs = Vec::new();
            let spec = rimz::agents::spending::HeadlineSpec::default();
            let req = rimz::agents::spending::WalkRequest {
                files: &files,
                prices: &fixture.prices,
                now_secs: SPENDING_NOW_SECS,
                origin_overrides: &origin_overrides,
                user_inputs: &user_inputs,
                scope: None,
                spec: &spec,
            };
            divan::black_box(fixture.walker.walk(
                &fixture.cache_path,
                &req,
                &mut rimz::agents::spending::SilentWalk,
            ));
        });
}

#[divan::bench(sample_count = 10, sample_size = 1, skip_ext_time)]
fn spending_live_scale_warm_discovery_only(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_discovery_fixture(true))
        .bench_local_values(|mut fixture| {
            divan::black_box(fixture.walker.discover_declared_spending_files(
                rimz::agents::definition_by_kind("claude").expect("Claude definition"),
                fixture.sources,
                SPENDING_NOW_SECS,
            ));
        });
}

#[divan::bench(sample_count = 3, sample_size = 1, skip_ext_time)]
fn spending_live_scale_additional_workspace_scope(bencher: Bencher) {
    bencher
        .with_inputs(|| spending_fixture_scaled(6_000, 17, true, true))
        .bench_local_values(|mut fixture| {
            let root = fixture._tempdir.path().to_path_buf();
            let scope = rimz::agents::spending::SpendScope::from_roots(Some(&root), &[]);
            let spec = rimz::agents::spending::HeadlineSpec::default();
            divan::black_box(rimz::testkit::spending_scope_from_warm_walker(
                &mut fixture.walker,
                &fixture.cache_path,
                &fixture.files,
                &scope,
                SPENDING_NOW_SECS,
                &spec,
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn enrich_cached(bencher: Bencher) {
    bencher
        .with_inputs(enrich_fixture)
        .bench_local_values(|fixture| {
            divan::black_box(rimz::sidebar::enrich::enrich(
                fixture.snapshot,
                Some(&fixture.frame),
                &fixture._workspace.paths,
                &fixture.runtime,
                None,
                None,
                rimz::sidebar::enrich::FoldOpts {
                    producing: false,
                    fresh_roots: None,
                    config: None,
                    lanes: None,
                    agent_projection: Default::default(),
                },
                &rimz::diag::DiagSink::disabled(),
            ));
        });
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn kimi_changed_session_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| changed_session_fixture(rimz::testkit::changed_kimi_session_fixture))
        .bench_local_values(|fixture| divan::black_box(fixture.refresh.refresh()));
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn grok_changed_session_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| changed_session_fixture(rimz::testkit::changed_grok_session_fixture))
        .bench_local_values(|fixture| divan::black_box(fixture.refresh.refresh()));
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn droid_changed_session_refresh(bencher: Bencher) {
    bencher
        .with_inputs(|| changed_session_fixture(rimz::testkit::changed_droid_session_fixture))
        .bench_local_values(|fixture| divan::black_box(fixture.refresh.refresh()));
}

#[divan::bench(sample_count = 20, sample_size = 1, skip_ext_time)]
fn render_fixed(bencher: Bencher) {
    bencher
        .with_inputs(snapshot_fixture)
        .bench_local_values(|fixture| {
            rimz::sidebar_pane::render::render_fixed(io::sink(), &fixture.snapshot, 54, 200)
                .expect("render");
        });
}
