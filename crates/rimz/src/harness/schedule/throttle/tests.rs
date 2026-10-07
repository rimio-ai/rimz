use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize};

use jiff::Timestamp;

use super::*;
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};

type SleepHook = Box<dyn FnMut(&FakeHost) + Send>;

/// A machine the test scripts: a queue in a tempdir, a clock that only
/// `sleep` moves, and readings the sleep hook may change as time passes.
pub(in crate::harness::schedule) struct FakeHost {
    dir: tempfile::TempDir,
    clock: AtomicU64,
    owner: Mutex<Owner>,
    dead: Mutex<Vec<u32>>,
    pub(in crate::harness::schedule) pressure: Mutex<Result<Pressure, String>>,
    memory: Mutex<Result<u64, String>>,
    disk: Mutex<u64>,
    disk_reads: Mutex<Vec<PathBuf>>,
    rooms: Mutex<BTreeMap<String, Result<Vec<AgentState>, String>>>,
    agent_reads: AtomicUsize,
    on_sleep: Mutex<Option<SleepHook>>,
    on_lock: Mutex<Option<SleepHook>>,
    /// Raised as Ctrl-C would be; only a hold that listens hears it.
    pub(in crate::harness::schedule) interrupt: Arc<AtomicBool>,
    listening: AtomicUsize,
}

impl FakeHost {
    pub(in crate::harness::schedule) fn new() -> Arc<Self> {
        Arc::new(Self {
            dir: tempfile::tempdir().expect("tempdir"),
            clock: AtomicU64::new(1_000_000),
            owner: Mutex::new(Owner {
                pid: 1,
                start: None,
            }),
            dead: Mutex::new(Vec::new()),
            pressure: Mutex::new(Ok(Pressure {
                avg10: 0.0,
                avg60: 0.0,
            })),
            memory: Mutex::new(Ok(u64::MAX)),
            disk: Mutex::new(u64::MAX),
            disk_reads: Mutex::new(Vec::new()),
            rooms: Mutex::new(BTreeMap::new()),
            agent_reads: AtomicUsize::new(0),
            on_sleep: Mutex::new(None),
            on_lock: Mutex::new(None),
            interrupt: Arc::new(AtomicBool::new(false)),
            listening: AtomicUsize::new(0),
        })
    }

    /// The seam as the gate takes it.
    pub(in crate::harness::schedule) fn host(self: &Arc<Self>) -> Arc<dyn Host> {
        Arc::clone(self) as Arc<dyn Host>
    }

    /// Run `hook` after every sleep, with the clock already advanced.
    pub(in crate::harness::schedule) fn on_sleep(
        &self,
        hook: impl FnMut(&FakeHost) + Send + 'static,
    ) {
        *self.on_sleep.lock().unwrap() = Some(Box::new(hook));
    }

    /// Run `hook` each time the queue lock is about to be taken.
    pub(in crate::harness::schedule) fn on_lock(
        &self,
        hook: impl FnMut(&FakeHost) + Send + 'static,
    ) {
        *self.on_lock.lock().unwrap() = Some(Box::new(hook));
    }

    /// Milliseconds since the fake clock started.
    pub(in crate::harness::schedule) fn elapsed_ms(&self) -> u64 {
        self.clock.load(Ordering::SeqCst) - 1_000_000
    }

    pub(in crate::harness::schedule) fn set_pressure(&self, avg10: f32, avg60: f32) {
        *self.pressure.lock().unwrap() = Ok(Pressure { avg10, avg60 });
    }

    pub(in crate::harness::schedule) fn tickets(&self) -> usize {
        ticket_paths(&self.queue_dir()).unwrap().len()
    }

    /// Every path free disk was read at, in order.
    pub(in crate::harness::schedule) fn disk_reads(&self) -> Vec<PathBuf> {
        self.disk_reads.lock().unwrap().clone()
    }

    /// Become another process, as a second `rimz loop run` would be.
    pub(in crate::harness::schedule) fn become_owner(&self, pid: u32) {
        self.owner.lock().unwrap().pid = pid;
    }

    fn kill(&self, pid: u32) {
        self.dead.lock().unwrap().push(pid);
    }

    fn set_agents(&self, workspace: &WorkspaceId, agents: Vec<AgentState>) {
        self.rooms
            .lock()
            .unwrap()
            .insert(workspace.to_string(), Ok(agents));
    }
}

impl Host for FakeHost {
    fn now_ms(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn sleep(&self, duration: Duration) {
        self.clock
            .fetch_add(duration_ms(duration), Ordering::SeqCst);
        let hook = self.on_sleep.lock().unwrap().take();
        if let Some(mut hook) = hook {
            hook(self);
            self.on_sleep.lock().unwrap().get_or_insert(hook);
        }
    }

    fn queue_dir(&self) -> PathBuf {
        self.dir.path().join("queue")
    }

    fn queue_lock(&self) -> PathBuf {
        let hook = self.on_lock.lock().unwrap().take();
        if let Some(mut hook) = hook {
            hook(self);
            self.on_lock.lock().unwrap().get_or_insert(hook);
        }
        self.dir.path().join("queue.lock")
    }

    fn owner(&self) -> Owner {
        self.owner.lock().unwrap().clone()
    }

    fn owner_is_live(&self, owner: &Owner) -> bool {
        !self.dead.lock().unwrap().contains(&owner.pid)
    }

    fn interrupts(&self) -> io::Result<Interrupts> {
        self.listening.fetch_add(1, Ordering::SeqCst);
        Ok(Interrupts::raised_by(Arc::clone(&self.interrupt)))
    }

    fn pressure(&self, _resource: Resource) -> Result<Pressure, String> {
        self.pressure.lock().unwrap().clone()
    }

    fn memory_available(&self) -> Result<u64, String> {
        self.memory.lock().unwrap().clone()
    }

    fn disk_free(&self, at: &Path) -> Result<u64, String> {
        self.disk_reads.lock().unwrap().push(at.to_path_buf());
        Ok(*self.disk.lock().unwrap())
    }

    fn workspaces(&self) -> Result<Vec<WorkspaceId>, String> {
        Ok(self
            .rooms
            .lock()
            .unwrap()
            .keys()
            .map(|id| WorkspaceId::parse(id).expect("workspace id"))
            .collect())
    }

    fn agents(&self, workspace: &WorkspaceId) -> Result<Vec<AgentState>, String> {
        self.agent_reads.fetch_add(1, Ordering::SeqCst);
        self.rooms
            .lock()
            .unwrap()
            .get(&workspace.to_string())
            .cloned()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    fn task_runs(&self, _workspace: &WorkspaceId, _task: &str) -> Result<Vec<RunRecord>, String> {
        Ok(Vec::new())
    }
}

fn config(text: &str) -> ThrottleConfig {
    toml::from_str(text).expect("throttle config")
}

#[test]
fn compact_load_reports_pressure_pairs_and_omits_unavailable_readings() {
    let host = FakeHost::new();
    host.set_pressure(14.0, 18.0);
    assert_eq!(
        compact_load_in(host.as_ref()).as_deref(),
        Some("cpu 14%/18% · io 14%/18% · memory 14%/18% (avg10/avg60)")
    );
    *host.pressure.lock().unwrap() = Err("unavailable".into());
    assert_eq!(compact_load_in(host.as_ref()), None);
}

fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(Path::new("/repo"))
}

fn run(task: &str) -> Run {
    Run {
        task: task.to_owned(),
        root: PathBuf::from("/repo"),
        checkout: PathBuf::from("/repo"),
        disk: PathBuf::from("/repo"),
        workspace: workspace(),
    }
}

fn agent(id: &str, status: AgentStatus) -> AgentState {
    AgentState {
        status,
        name: Some(id.to_owned()),
        ..crate::testkit::agent_state("claude", id, Timestamp::now())
    }
}

/// The row a launch commit writes: its own launch reference, mid-turn.
fn launched(reference: &str) -> AgentState {
    AgentState {
        launch_id: Some(AgentSessionId::from(reference)),
        ..agent(reference, AgentStatus::Running)
    }
}

fn admitted(admission: Admission) -> (Turn, Option<Duration>) {
    match admission {
        Admission::Turn { turn, waited } => (turn, waited),
        other => panic!("expected a turn, got {other:?}"),
    }
}

fn admit_quiet(fake: &Arc<FakeHost>, config: &ThrottleConfig, run: &Run) -> Admission {
    admit(&fake.host(), config, run, None, &mut |_| {}).expect("admit")
}

/// One pass for a ticket that is not this thread's, as its owner would run it.
fn step(fake: &FakeHost, config: &ThrottleConfig, run: &Run, ticket: &Path) -> Option<String> {
    pass(fake, config, run, ticket, fake.now_ms(), &mut None)
        .expect("pass")
        .map(|hold| hold.text)
}

fn enqueue_as(fake: &FakeHost, pid: u32, run: &Run) -> PathBuf {
    fake.become_owner(pid);
    fake.clock.fetch_add(1, Ordering::SeqCst);
    enqueue(fake, run, fake.now_ms()).expect("enqueue")
}

#[test]
fn pressure_parses_the_some_line_of_real_proc_text() {
    let text = "some avg10=22.92 avg60=23.06 avg300=31.40 total=1234567\n\
                full avg10=0.00 avg60=1.50 avg300=2.00 total=99\n";
    assert_eq!(
        parse_pressure(text),
        Some(Pressure {
            avg10: 22.92,
            avg60: 23.06
        })
    );
    // `/proc/pressure/cpu` on an older kernel carries only the `some` line.
    assert_eq!(
        parse_pressure("some avg10=0.00 avg60=0.10 avg300=0.05 total=5\n"),
        Some(Pressure {
            avg10: 0.0,
            avg60: 0.1
        })
    );
    assert_eq!(parse_pressure("full avg10=0.00 avg60=0.00\n"), None);
    assert_eq!(parse_pressure(""), None);
}

#[test]
fn nothing_configured_writes_no_ticket() {
    let fake = FakeHost::new();
    let open = admit_quiet(&fake, &config("pace = \"0s\"\n"), &run("nightly"));
    assert!(matches!(open, Admission::Open), "{open:?}");
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn a_pressure_limit_holds_until_both_averages_are_under_it() {
    let fake = FakeHost::new();
    fake.set_pressure(10.0, 41.0);
    fake.on_sleep(|fake| match fake.elapsed_ms() {
        // The short average alone over the limit still holds.
        6_000 => fake.set_pressure(30.0, 12.0),
        12_000 => fake.set_pressure(24.0, 12.0),
        _ => {}
    });
    let mut heard = Vec::new();
    let admission = admit(
        &fake.host(),
        &config("cpu-pressure = 25\n"),
        &run("nightly"),
        None,
        &mut |reason| heard.push(reason.to_owned()),
    )
    .expect("admit");
    let (_turn, waited) = admitted(admission);
    assert_eq!(
        heard,
        ["cpu pressure 41% >= 25%", "cpu pressure 30% >= 25%"],
        "each new reason is heard once"
    );
    // Sampled at 0s, 5s, 10s, 15s: the reading cleared at 12s admits at 15s.
    assert_eq!(fake.elapsed_ms(), 15_000);
    assert_eq!(waited, Some(Duration::from_secs(15)));
}

#[test]
fn a_run_held_past_max_wait_is_skipped_with_its_last_reason() {
    let fake = FakeHost::new();
    fake.set_pressure(90.0, 90.0);
    let skipped = admit_quiet(
        &fake,
        &config("cpu-pressure = 25\nmax-wait = \"3s\"\n"),
        &run("nightly"),
    );
    let Admission::Skipped { reason } = skipped else {
        panic!("expected a skip, got {skipped:?}");
    };
    assert_eq!(reason, "cpu pressure 90% >= 25%; held 3s");
    assert_eq!(fake.elapsed_ms(), 3_000);
    assert_eq!(fake.tickets(), 0, "a skipped run leaves the queue");
}

#[test]
fn the_next_start_waits_for_the_report_then_the_provider_or_the_ceiling() {
    for (observed_at, admitted_at) in [(Some(5_000), 5_000), (None, 13_000)] {
        let fake = FakeHost::new();
        let pacing = config("");
        let (first, waited) = admitted(admit_quiet(&fake, &pacing, &run("first")));
        assert_eq!(waited, None, "an empty queue admits at once");

        fake.set_agents(&workspace(), Vec::new());
        fake.on_sleep(move |fake| {
            let at = fake.elapsed_ms();
            if at == 3_000 {
                // The launch commits its row, then reports.
                fake.set_agents(&workspace(), vec![launched("launch_a")]);
                first.report_launch(&workspace(), &AgentSessionId::from("launch_a"));
                // A verify retry reports again; the commit time must not move.
                first.report_launch(&workspace(), &AgentSessionId::from("launch_b"));
            }
            if Some(at) == observed_at {
                let mut adopted = launched("provider-session");
                adopted.launch_id = Some(AgentSessionId::from("launch_a"));
                fake.set_agents(&workspace(), vec![adopted]);
            }
        });
        fake.become_owner(2);
        let mut heard = Vec::new();
        let admission = admit(&fake.host(), &pacing, &run("second"), None, &mut |reason| {
            heard.push(reason.to_owned());
        })
        .expect("admit");
        let (_second, waited) = admitted(admission);
        assert_eq!(heard, ["1 start ahead"]);
        assert_eq!(
            fake.elapsed_ms(),
            admitted_at,
            "observed at {observed_at:?}"
        );
        assert_eq!(waited, Some(Duration::from_millis(admitted_at)));
        assert_eq!(fake.tickets(), 1, "the released turn's ticket is gone");
    }
}

#[test]
fn a_launch_that_ended_or_vanished_passes_the_turn_on() {
    let reference = AgentSessionId::from("launch_a");
    assert!(!provider_reported(&[launched("launch_a")], &reference));
    let mut ended = launched("launch_a");
    ended.ended_at = Some(Timestamp::now());
    assert!(provider_reported(&[ended], &reference));
    assert!(provider_reported(
        &[AgentState {
            launch_id: Some(reference.clone()),
            ..agent("launch_a", AgentStatus::Failed)
        }],
        &reference
    ));
    assert!(provider_reported(&[], &reference));
    assert!(!provider_reported(
        &[launched("launch_a"), launched("launch_other")],
        &reference
    ));
}

#[test]
fn with_pacing_off_the_next_start_is_admitted_at_the_report_and_counts_its_row() {
    let fake = FakeHost::new();
    let capped = config("pace = \"0s\"\nmax-active = 1\nmax-wait = \"20s\"\n");
    fake.set_agents(&workspace(), Vec::new());
    let (first, _) = admitted(admit_quiet(&fake, &capped, &run("first")));
    fake.on_sleep(move |fake| {
        if fake.elapsed_ms() == 2_000 {
            fake.set_agents(&workspace(), vec![launched("launch_a")]);
            first.report_launch(&workspace(), &AgentSessionId::from("launch_a"));
        }
    });
    fake.become_owner(2);
    let mut heard = Vec::new();
    let skipped = admit(&fake.host(), &capped, &run("second"), None, &mut |reason| {
        heard.push((fake.elapsed_ms(), reason.to_owned()));
    })
    .expect("admit");
    // Before the report the turn is held; at the report it passes on, and the
    // first sample the second run takes already counts the committed row.
    assert_eq!(
        heard,
        [
            (0, "1 start ahead".to_owned()),
            (2_000, "active agents 1 >= 1 (@claude)".to_owned()),
        ]
    );
    assert!(matches!(skipped, Admission::Skipped { .. }), "{skipped:?}");
}

#[test]
fn a_dropped_turn_and_a_dead_owner_both_release_the_next_run() {
    let fake = FakeHost::new();
    let pacing = config("");
    let (dropped, _) = admitted(admit_quiet(&fake, &pacing, &run("first")));
    drop(dropped);
    assert_eq!(fake.tickets(), 0, "an unreported turn removes its ticket");

    fake.become_owner(2);
    let (orphan, _) = admitted(admit_quiet(&fake, &pacing, &run("second")));
    // The owner dies holding the turn: nothing runs its drop.
    std::mem::forget(orphan);
    fake.become_owner(3);
    let waiting = enqueue_as(&fake, 3, &run("third"));
    assert_eq!(
        step(&fake, &pacing, &run("third"), &waiting).as_deref(),
        Some("1 start ahead")
    );
    fake.kill(2);
    assert_eq!(step(&fake, &pacing, &run("third"), &waiting), None);
    assert_eq!(fake.tickets(), 1);
}

#[test]
fn the_machine_cap_counts_only_working_agents_and_samples_every_five_seconds() {
    let fake = FakeHost::new();
    let capped = config("pace = \"0s\"\nmax-active = 2\n");
    let other = WorkspaceId::from_project_root(Path::new("/other"));
    let mut ended = agent("ended", AgentStatus::Running);
    ended.ended_at = Some(Timestamp::now());
    let mut dead = agent("dead", AgentStatus::Running);
    dead.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "dead",
        u32::MAX,
        None,
    ));
    fake.set_agents(
        &workspace(),
        vec![
            agent("otter", AgentStatus::Running),
            agent("asking", AgentStatus::Waiting),
            agent("resting", AgentStatus::Idle),
            agent("done", AgentStatus::Success),
            ended,
            dead,
        ],
    );
    fake.set_agents(&other, vec![agent("fox", AgentStatus::Running)]);
    fake.on_sleep(move |fake| {
        if fake.elapsed_ms() == 7_000 {
            fake.set_agents(&other, vec![agent("fox", AgentStatus::Success)]);
        }
    });
    let mut heard = Vec::new();
    let admission = admit(
        &fake.host(),
        &capped,
        &run("nightly"),
        None,
        &mut |reason| {
            heard.push(reason.to_owned());
        },
    )
    .expect("admit");
    admitted(admission);
    // A lone agent is addressed by its kind; one among peers by its name.
    assert_eq!(heard, ["active agents 2 >= 2 (@otter, @claude)"]);
    assert_eq!(fake.elapsed_ms(), 10_000, "admitted on the sample after 7s");
    // Three samples (0s, 5s, 10s) over two workspaces, not one per recheck.
    assert_eq!(fake.agent_reads.load(Ordering::SeqCst), 6);
}

#[test]
fn a_cap_reason_truncates_the_agents_it_names() {
    let fake = FakeHost::new();
    fake.set_agents(
        &workspace(),
        ["a", "b", "c", "d"]
            .map(|id| agent(id, AgentStatus::Running))
            .to_vec(),
    );
    let hold = limit_hold(fake.as_ref(), &config("max-active = 3\n"), &run("nightly"))
        .expect("sample")
        .expect("held");
    assert_eq!(hold.text, "active agents 4 >= 3 (@a, @b, +2)");
}

#[test]
fn an_unreadable_workspace_holds_the_run_and_names_it() {
    let fake = FakeHost::new();
    fake.rooms.lock().unwrap().insert(
        workspace().to_string(),
        Err("workspace ws_broken unreadable: torn log".to_owned()),
    );
    let hold = limit_hold(fake.as_ref(), &config("max-active = 3\n"), &run("nightly"))
        .expect("sample")
        .expect("held, never a short count");
    assert_eq!(
        hold.text,
        "cannot count active agents: workspace ws_broken unreadable: torn log"
    );
}

#[test]
fn a_run_over_its_own_task_cap_is_passed_by_another_task_and_keeps_its_place() {
    let fake = FakeHost::new();
    let capped = config("pace = \"0s\"\nmax-active-per-task = 1\n");
    let mut fixer = agent("fixer-1", AgentStatus::Running);
    fixer.loop_task = Some("fixer".to_owned());
    // Another root's task of the same name is another task: its row lives in
    // another workspace and is never read for this cap.
    let mut elsewhere = agent("fixer-far", AgentStatus::Running);
    elsewhere.loop_task = Some("fixer".to_owned());
    fake.set_agents(
        &workspace(),
        vec![fixer.clone(), agent("bystander", AgentStatus::Running)],
    );
    fake.set_agents(
        &WorkspaceId::from_project_root(Path::new("/other")),
        vec![elsewhere],
    );

    let first_fixer = enqueue_as(&fake, 1, &run("fixer"));
    let other_task = enqueue_as(&fake, 2, &run("nightly"));
    let second_fixer = enqueue_as(&fake, 3, &run("fixer"));

    assert_eq!(
        step(&fake, &capped, &run("nightly"), &other_task).as_deref(),
        Some("1 start ahead"),
        "nothing is stepped over before it recorded its own cap"
    );
    assert_eq!(
        step(&fake, &capped, &run("fixer"), &first_fixer).as_deref(),
        Some("task's active agents 1 >= 1 (@fixer-1)")
    );
    assert_eq!(
        step(&fake, &capped, &run("fixer"), &second_fixer).as_deref(),
        Some("2 starts ahead"),
        "a run of the same task stays behind it"
    );
    assert_eq!(step(&fake, &capped, &run("nightly"), &other_task), None);
    remove_ticket(&other_task);

    fixer.status = AgentStatus::Success;
    fake.set_agents(&workspace(), vec![fixer]);
    assert_eq!(
        step(&fake, &capped, &run("fixer"), &second_fixer).as_deref(),
        Some("1 start ahead")
    );
    assert_eq!(step(&fake, &capped, &run("fixer"), &first_fixer), None);
}

#[test]
fn queue_order_follows_enqueue_order_across_owners() {
    let fake = FakeHost::new();
    let pacing = config("");
    let tickets = [1, 2, 3].map(|pid| enqueue_as(&fake, pid, &run(&format!("task-{pid}"))));
    let step_for = |index: usize| {
        step(
            &fake,
            &pacing,
            &run(&format!("task-{}", index + 1)),
            &tickets[index],
        )
    };
    assert_eq!(step_for(2).as_deref(), Some("2 starts ahead"));
    assert_eq!(step_for(1).as_deref(), Some("1 start ahead"));
    assert_eq!(step_for(0), None);
    assert_eq!(
        step_for(1).as_deref(),
        Some("1 start ahead"),
        "the turn is held"
    );
    remove_ticket(&tickets[0]);
    assert_eq!(step_for(2).as_deref(), Some("1 start ahead"));
    assert_eq!(step_for(1), None);
}

#[test]
fn a_waiting_run_is_reported_with_its_reason_and_place() {
    let fake = FakeHost::new();
    let pacing = config("");
    let (_turn, _) = admitted(admit_quiet(&fake, &pacing, &run("first")));
    let waiting = enqueue_as(&fake, 2, &run("second"));
    let since_ms = fake.now_ms();
    let held = |reason: Option<&str>, pid, position| Held {
        pid,
        checkout: PathBuf::from("/repo"),
        since_ms,
        reason: reason.map(str::to_owned),
        position,
    };
    assert_eq!(
        held_in(fake.as_ref(), "second", Path::new("/repo")),
        [held(None, 2, 2)]
    );
    step(&fake, &pacing, &run("second"), &waiting);
    // Every waiting run of the task is listed, each with its own process.
    let mut later = held(None, 3, 3);
    enqueue_as(&fake, 3, &run("second"));
    later.since_ms = fake.now_ms();
    assert_eq!(
        held_in(fake.as_ref(), "second", Path::new("/repo")),
        [held(Some("1 start ahead"), 2, 2), later.clone()]
    );
    assert_eq!(held_in(fake.as_ref(), "first", Path::new("/repo")), []);
    assert_eq!(
        held_in(fake.as_ref(), "second", Path::new("/elsewhere")),
        []
    );
    fake.kill(2);
    assert_eq!(
        held_in(fake.as_ref(), "second", Path::new("/repo")),
        [later]
    );
}

#[test]
fn a_configured_limit_this_host_cannot_read_fails_with_the_key_and_the_fix() {
    let fake = FakeHost::new();
    *fake.pressure.lock().unwrap() = Err("/proc/pressure/io: No such file".to_owned());
    assert_eq!(unreadable(fake.as_ref(), &config("max-active = 2\n")), None);
    let problem = unreadable(fake.as_ref(), &config("io-pressure = 40\n")).expect("refused");
    assert!(problem.contains("`loop.throttle.io-pressure`"), "{problem}");
    assert!(problem.contains("/proc/pressure/io"), "{problem}");
    assert!(problem.contains("psi=1"), "{problem}");
    let error = admit(
        &fake.host(),
        &config("io-pressure = 40\n"),
        &run("nightly"),
        None,
        &mut |_| {},
    )
    .expect_err("a gated fire errors");
    assert!(error.to_string().contains("`loop.throttle.io-pressure`"));
    assert_eq!(fake.tickets(), 0);

    *fake.memory.lock().unwrap() = Err("/proc/meminfo: not found".to_owned());
    let problem = unreadable(fake.as_ref(), &config("min-memory = \"8GB\"\n")).expect("refused");
    assert!(problem.contains("`loop.throttle.min-memory`"), "{problem}");
}

#[test]
fn memory_and_disk_floors_hold_below_the_floor_and_show_beside_the_reading() {
    let fake = FakeHost::new();
    let floors = config("min-memory = \"8GB\"\nmin-disk = \"20GB\"\n");
    *fake.memory.lock().unwrap() = Ok(3_000_000_000);
    let hold = limit_hold(fake.as_ref(), &floors, &run("nightly")).unwrap();
    assert_eq!(hold.unwrap().text, "available memory 3 GB < 8 GB");
    *fake.memory.lock().unwrap() = Ok(9_000_000_000);
    *fake.disk.lock().unwrap() = 12_000_000_000;
    let hold = limit_hold(fake.as_ref(), &floors, &run("nightly")).unwrap();
    assert_eq!(hold.unwrap().text, "free disk 12 GB < 20 GB at /repo");
    *fake.disk.lock().unwrap() = 20_000_000_000;
    assert_eq!(
        limit_hold(fake.as_ref(), &floors, &run("nightly")).unwrap(),
        None
    );

    fake.set_pressure(2.4, 3.6);
    let mut mine = agent("fixer-1", AgentStatus::Running);
    mine.loop_task = Some("nightly".to_owned());
    fake.set_agents(
        &workspace(),
        vec![agent("otter", AgentStatus::Running), mine],
    );
    let rows = readings_in(
        fake.as_ref(),
        &config("cpu-pressure = 60\nmax-active = 6\nmax-active-per-task = 2\n"),
        &run("nightly"),
    );
    assert_eq!(
        rows,
        [
            ("cpu pressure", "avg10 2% · avg60 4% (limit 60%)".to_owned()),
            ("io pressure", "avg10 2% · avg60 4%".to_owned()),
            ("memory pressure", "avg10 2% · avg60 4%".to_owned()),
            ("memory", "9 GB available".to_owned()),
            ("disk", "20 GB free".to_owned()),
            ("active agents", "2 (limit max 6)".to_owned()),
            ("task's active agents", "1 (limit max 2)".to_owned()),
        ]
    );
}

#[test]
fn a_held_run_is_skipped_at_max_wait_even_when_its_hold_clears_then() {
    // The holder's turn passes on exactly at the deadline, or a slow poll
    // first wakes past it: both are a skip, never a late start.
    for (clears_at, delay_ms) in [(3_000, 0), (1_000, 5_000)] {
        let fake = FakeHost::new();
        let paced = config("max-wait = \"3s\"\n");
        let mut holder = Some(admitted(admit_quiet(&fake, &paced, &run("first"))).0);
        fake.on_sleep(move |fake| {
            if fake.elapsed_ms() == clears_at {
                fake.clock.fetch_add(delay_ms, Ordering::SeqCst);
                holder.take();
            }
        });
        fake.become_owner(2);
        let skipped = admit_quiet(&fake, &paced, &run("second"));
        let Admission::Skipped { reason } = skipped else {
            panic!("cleared at {clears_at}ms: expected a skip, got {skipped:?}");
        };
        assert_eq!(reason, "1 start ahead; held 3s");
        assert_eq!(fake.elapsed_ms(), clears_at + delay_ms);
        assert_eq!(fake.tickets(), 0);
    }
}

#[test]
fn a_compacting_agent_counts_toward_neither_cap_until_its_window_expires() {
    for capped in ["max-active = 1\n", "max-active-per-task = 1\n"] {
        let fake = FakeHost::new();
        let now = Timestamp::from_millisecond(i64::try_from(fake.now_ms()).unwrap()).unwrap();
        let compacting = |ago_secs: i64| AgentState {
            loop_task: Some("nightly".to_owned()),
            compacting_since: Some(now - jiff::SignedDuration::from_secs(ago_secs)),
            ..agent("otter", AgentStatus::Running)
        };
        fake.set_agents(&workspace(), vec![compacting(10)]);
        assert_eq!(
            limit_hold(fake.as_ref(), &config(capped), &run("nightly")).unwrap(),
            None,
            "{capped}"
        );
        fake.set_agents(
            &workspace(),
            vec![compacting(crate::agents::COMPACTING_WINDOW_SECS + 1)],
        );
        let hold = limit_hold(fake.as_ref(), &config(capped), &run("nightly"))
            .unwrap()
            .unwrap_or_else(|| panic!("{capped}: a stale compaction counts"));
        assert!(hold.text.ends_with(">= 1 (@claude)"), "{}", hold.text);
    }
}

#[test]
fn a_corrupt_ticket_is_removed_and_the_next_run_is_admitted() {
    let fake = FakeHost::new();
    let corrupt = enqueue_as(&fake, 1, &run("first"));
    std::fs::write(&corrupt, b"{\"owner\":").unwrap();
    fake.become_owner(2);
    let (turn, _) = admitted(admit_quiet(&fake, &config(""), &run("second")));
    assert!(!corrupt.exists());
    assert_eq!(fake.tickets(), 1, "only the admitted turn is left");
    drop(turn);
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn an_existing_ticket_that_cannot_be_read_fails_admission() {
    let fake = FakeHost::new();
    let unreadable = enqueue_as(&fake, 1, &run("first"));
    std::fs::remove_file(&unreadable).unwrap();
    std::fs::create_dir(&unreadable).unwrap();
    fake.become_owner(2);
    let error = admit(&fake.host(), &config(""), &run("second"), None, &mut |_| {})
        .expect_err("an unreadable ticket may be a held turn");
    assert!(matches!(error, ThrottleError::Io { path, .. } if path == unreadable));
    assert_eq!(fake.tickets(), 1, "only the unreadable ticket is left");
}

#[test]
fn every_waiting_run_of_a_capped_task_is_stepped_over_by_another_task() {
    let fake = FakeHost::new();
    let capped = config("pace = \"0s\"\nmax-active-per-task = 1\ncpu-pressure = 50\n");
    let mut fixer = agent("fixer-1", AgentStatus::Running);
    fixer.loop_task = Some("fixer".to_owned());
    fake.set_agents(&workspace(), vec![fixer]);

    // A1, A2, B: A2 is held behind A1 and never records the cap itself.
    let first_fixer = enqueue_as(&fake, 1, &run("fixer"));
    let second_fixer = enqueue_as(&fake, 2, &run("fixer"));
    let other_task = enqueue_as(&fake, 3, &run("nightly"));
    assert_eq!(
        step(&fake, &capped, &run("fixer"), &first_fixer).as_deref(),
        Some("task's active agents 1 >= 1 (@claude)")
    );
    // A2 has not passed yet, so it carries no reason.
    assert_eq!(step(&fake, &capped, &run("nightly"), &other_task), None);
    remove_ticket(&other_task);

    let other_task = enqueue_as(&fake, 3, &run("nightly"));
    assert_eq!(
        step(&fake, &capped, &run("fixer"), &second_fixer).as_deref(),
        Some("1 start ahead"),
        "a run of the capped task keeps its place behind the first"
    );
    assert_eq!(step(&fake, &capped, &run("nightly"), &other_task), None);
    remove_ticket(&other_task);

    // A run held by a machine-wide limit is never stepped over.
    let pressured = enqueue_as(&fake, 4, &run("nightly"));
    let last = enqueue_as(&fake, 5, &run("weekly"));
    fake.set_pressure(80.0, 80.0);
    assert_eq!(
        step(&fake, &capped, &run("nightly"), &pressured).as_deref(),
        Some("cpu pressure 80% >= 50%")
    );
    fake.set_pressure(0.0, 0.0);
    assert_eq!(
        step(&fake, &capped, &run("weekly"), &last).as_deref(),
        Some("1 start ahead")
    );
}

#[test]
fn ctrl_c_ends_a_hold_it_listens_for_and_leaves_no_ticket() {
    let fake = FakeHost::new();
    let pacing = config("max-wait = \"1m\"\n");
    let (_first, _) = admitted(admit_quiet(&fake, &pacing, &run("first")));
    assert_eq!(
        fake.listening.load(Ordering::SeqCst),
        0,
        "a run never held never listens"
    );
    let raised = Arc::clone(&fake.interrupt);
    fake.on_sleep(move |fake| {
        if fake.elapsed_ms() == 2_000 {
            raised.store(true, Ordering::SeqCst);
        }
    });
    fake.become_owner(2);
    let mut listening_when_heard = Vec::new();
    let interrupted = admit(&fake.host(), &pacing, &run("second"), None, &mut |_| {
        listening_when_heard.push(fake.listening.load(Ordering::SeqCst));
    })
    .expect("admit");
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
    assert_eq!(
        listening_when_heard,
        [1],
        "the hold listens before it is announced"
    );
    assert_eq!(fake.elapsed_ms(), 2_000);
    assert_eq!(fake.listening.load(Ordering::SeqCst), 1);
    assert_eq!(fake.tickets(), 1, "only the first run's turn is left");
}

/// A run whose caller already listens is the one held behind `first`.
fn admit_listening(fake: &Arc<FakeHost>, raised: &Arc<AtomicBool>) -> (Admission, Vec<String>) {
    let pacing = config("max-wait = \"1m\"\n");
    let (first, _) = admitted(admit_quiet(fake, &pacing, &run("first")));
    fake.become_owner(2);
    let mut heard = Vec::new();
    let admission = admit(
        &fake.host(),
        &pacing,
        &run("second"),
        Some(Interrupts::raised_by(Arc::clone(raised))),
        &mut |reason| heard.push(reason.to_owned()),
    )
    .expect("admit");
    drop(first);
    (admission, heard)
}

#[test]
fn ctrl_c_heard_before_admission_ends_the_run_on_its_first_pass() {
    let fake = FakeHost::new();
    let (interrupted, heard) = admit_listening(&fake, &Arc::new(AtomicBool::new(true)));
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
    assert_eq!(fake.elapsed_ms(), 0, "no recheck was slept");
    assert!(heard.is_empty(), "no hold was announced: {heard:?}");
    assert_eq!(fake.listening.load(Ordering::SeqCst), 0);
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn a_held_run_keeps_the_listener_it_was_given() {
    let fake = FakeHost::new();
    let raised = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&raised);
    fake.on_sleep(move |fake| {
        if fake.elapsed_ms() == 2_000 {
            flag.store(true, Ordering::SeqCst);
        }
    });
    let (interrupted, heard) = admit_listening(&fake, &raised);
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
    assert_eq!(heard, ["1 start ahead"]);
    assert_eq!(fake.elapsed_ms(), 2_000);
    assert_eq!(
        fake.listening.load(Ordering::SeqCst),
        0,
        "a run given a listener registers no second one"
    );
}

#[test]
fn ctrl_c_raised_while_taking_the_queue_lock_wins_over_an_admission() {
    let fake = FakeHost::new();
    let pressure = config("pace = \"0s\"\nmax-wait = \"1m\"\ncpu-pressure = 50\n");
    fake.set_pressure(80.0, 80.0);
    fake.on_sleep(|fake| fake.set_pressure(0.0, 0.0));
    // The cleared reading is sampled at 5s, in the pass this lock opens.
    let raised = Arc::clone(&fake.interrupt);
    fake.on_lock(move |fake| {
        if fake.elapsed_ms() == 5_000 {
            raised.store(true, Ordering::SeqCst);
        }
    });
    let interrupted = admit_quiet(&fake, &pressure, &run("held"));
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
    assert_eq!(fake.tickets(), 0);
}
