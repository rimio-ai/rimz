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
    workflows: Mutex<Vec<crate::store::workflow::RunRecord>>,
    agent_reads: AtomicUsize,
    on_sleep: Mutex<Option<SleepHook>>,
    on_lock: Mutex<Option<SleepHook>>,
    on_check: Mutex<Option<SleepHook>>,
    check_answer: Mutex<Result<(Option<i32>, bool), String>>,
    checks: Mutex<Vec<StartGroup>>,
    check_timeouts: Mutex<Vec<Duration>>,
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
            workflows: Mutex::new(Vec::new()),
            agent_reads: AtomicUsize::new(0),
            on_sleep: Mutex::new(None),
            on_lock: Mutex::new(None),
            on_check: Mutex::new(None),
            check_answer: Mutex::new(Ok((Some(0), false))),
            checks: Mutex::new(Vec::new()),
            check_timeouts: Mutex::new(Vec::new()),
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

    pub(in crate::harness::schedule) fn checked_groups(&self) -> Vec<StartGroup> {
        self.checks.lock().unwrap().clone()
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
    fn check_cache(&self) -> PathBuf {
        self.dir.path().join("checks.json")
    }

    fn check(
        &self,
        _config: &ThrottleConfig,
        _run: &Run,
        group: &StartGroup,
        timeout: Duration,
    ) -> Result<super::super::runner::CheckOutcome, String> {
        self.checks.lock().unwrap().push(group.clone());
        self.check_timeouts.lock().unwrap().push(timeout);
        if let Some(mut hook) = self.on_check.lock().unwrap().take() {
            hook(self);
        }
        self.check_answer
            .lock()
            .unwrap()
            .clone()
            .map(|(code, timed_out)| {
                super::super::runner::CheckOutcome::new(
                    code == Some(0),
                    timed_out,
                    String::new(),
                    code,
                )
            })
    }

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

    fn task_workflows(
        &self,
        _workspace: &WorkspaceId,
        _task: &str,
    ) -> Result<Vec<crate::store::workflow::RunRecord>, String> {
        Ok(self.workflows.lock().unwrap().clone())
    }
}

fn config(text: &str) -> ThrottleConfig {
    toml::from_str(text).expect("throttle config")
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
        groups: vec![StartGroup {
            kind: AgentKind::new_unchecked("claude"),
            account: None,
            agents: 1,
        }],
        gives_up_ms: None,
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
    let run = Run {
        gives_up_ms: run.gives_up_ms.or(Some(fake.now_ms() + 1_800_000)),
        ..run.clone()
    };
    admit(&fake.host(), config, &run, None).expect("admit")
}

/// One pass for a ticket that is not this thread's, as its owner would run it.
fn step(fake: &FakeHost, config: &ThrottleConfig, run: &Run, ticket: &Path) -> Option<String> {
    pass(fake, config, run, Some(ticket), fake.now_ms(), &mut None)
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
fn capacity_task_cap_counts_working_agents_of_live_task_workflows() {
    let id = crate::ids::WorkflowRunId::new();
    let workflow: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":1, "id":id, "workflow":"review", "task":"nightly", "spec_dir":"/defs",
        "spec_digest":"test", "scope":{"kind":"project", "path":"/project"},
        "project_root":"/project", "created_at":Timestamp::UNIX_EPOCH,
        "entered_at":Timestamp::UNIX_EPOCH, "node":"build", "state":"active",
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1
    }))
    .unwrap();
    let mut working = agent("builder", AgentStatus::Running);
    working.workflow = Some(crate::agents::WorkflowAttemptRef {
        run: id,
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Step,
    });
    let agents = vec![working];
    let mut actual = Vec::new();
    for (task, state) in [
        ("nightly", crate::store::workflow::RunState::Active),
        ("other", crate::store::workflow::RunState::Active),
        ("nightly", crate::store::workflow::RunState::Done),
    ] {
        let mut run = workflow.clone();
        run.state = state;
        let counted = task_working(&agents, &[], &[run.clone()], task, Timestamp::now()).len();
        let fake = FakeHost::new();
        let candidate = self::run(task);
        fake.set_agents(&candidate.workspace, agents.clone());
        *fake.workflows.lock().unwrap() = vec![run];
        let held = limit_hold(fake.as_ref(), &config("max-active-per-task=1"), &candidate)
            .unwrap()
            .is_some();
        actual.push((counted, held));
    }
    assert_eq!(actual, vec![(1, true), (0, false), (0, false)]);
}

#[test]
fn capacity_nonblocking_holds_leave_no_ticket_and_do_not_check_behind_limits() {
    let results: Vec<_> = ["queue", "limit", "full", "error"]
        .into_iter()
        .map(|case| {
            let fake = FakeHost::new();
            let mut configured = config("pace = '0s'\ncheck = 'capacity'\n");
            let _holder = if case == "queue" {
                Some(admitted(admit_quiet(&fake, &config(""), &run("first"))).0)
            } else {
                None
            };
            match case {
                "limit" => {
                    configured.min_memory = Some("1GB".into());
                    *fake.memory.lock().unwrap() = Ok(0);
                }
                "full" => *fake.check_answer.lock().unwrap() = Ok((Some(75), false)),
                "error" => *fake.check_answer.lock().unwrap() = Ok((Some(2), false)),
                _ => {}
            }
            let before = fake.tickets();
            let attempt = try_admit_in(&fake.host(), &configured, &run("next")).unwrap();
            let kind = match attempt {
                Attempt::Held(hold) => Some(hold.kind),
                _ => None,
            };
            (
                kind,
                fake.tickets() == before,
                fake.checks.lock().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        results,
        [
            (Some(HoldKind::Queue), true, 0),
            (Some(HoldKind::Memory), true, 0),
            (Some(HoldKind::Capacity), true, 1),
            (Some(HoldKind::CheckFailed), true, 1),
        ]
    );
}

#[test]
fn capacity_system_check_receives_the_room_pin_checkout_and_group_environment() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = run("nightly");
    run.root = dir.path().to_owned();
    run.checkout = run.root.clone();
    run.groups[0].account = Some("work".parse().unwrap());
    run.groups[0].agents = 2;
    let configured = ThrottleConfig {
        check: Some(
            concat!(
                "test \"$PWD\" = \"$RIMZ_PROJECT_ROOT\" && ",
                "test -n \"$RIMZ_WORKSPACE_ID\" && ",
                "test \"$RIMZ_START_KIND\" = claude && ",
                "test \"$RIMZ_START_ACCOUNT\" = work && ",
                "test \"$RIMZ_START_AGENTS\" = 2 && ",
                "test \"$RIMZ_START_TASK\" = nightly"
            )
            .into(),
        ),
        ..Default::default()
    };
    let checked = SystemHost.check(
        &configured,
        &run,
        &run.groups[0],
        configured.check_timeout(),
    );
    assert!(
        checked.is_ok_and(|checked| checked.passed()),
        "the check must receive every launch field"
    );
}

#[test]
fn capacity_check_holds_an_admitted_turn_without_holding_the_queue_lock() {
    let fake = FakeHost::new();
    let weak = Arc::downgrade(&fake);
    *fake.on_check.lock().unwrap() = Some(Box::new(move |fake| {
        let lock = WorkspaceLock::try_acquire(&fake.queue_lock()).unwrap();
        assert!(
            lock.is_some(),
            "the script must not run under the queue lock"
        );
        drop(lock);
        let paths = ticket_paths(&fake.queue_dir()).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(
            read_ticket(&paths[0]).unwrap().unwrap().state,
            TicketState::Admitted
        );
        let second =
            try_admit_in(&weak.upgrade().unwrap().host(), &config(""), &run("second")).unwrap();
        assert!(
            matches!(&second, Attempt::Held(hold) if hold.text == "1 start ahead"),
            "{second:?}"
        );
        assert_eq!(fake.tickets(), 1);
    }));
    let attempt = try_admit_in(&fake.host(), &config("check = 'capacity'"), &run("first")).unwrap();
    assert!(matches!(attempt, Attempt::Turn(_)), "{attempt:?}");
    assert_eq!(fake.checks.lock().unwrap().len(), 1);
    drop(attempt);
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn capacity_answers_and_cache_distinguish_full_errors_and_success() {
    let answers = [
        Ok((Some(0), false)),
        Ok((Some(75), false)),
        Ok((Some(2), false)),
        Ok((None, true)),
        Ok((None, false)),
        Err("cannot spawn".to_owned()),
    ];
    let results: Vec<_> = answers
        .into_iter()
        .map(|answer| {
            let fake = FakeHost::new();
            *fake.check_answer.lock().unwrap() = answer;
            let configured = config("pace = '0s'\ncheck = 'capacity'");
            let mut kinds = Vec::new();
            for _ in 0..2 {
                let attempt = try_admit_in(&fake.host(), &configured, &run("first")).unwrap();
                kinds.push(match attempt {
                    Attempt::Turn(_) => None,
                    Attempt::Held(hold) => Some(hold.kind),
                    Attempt::Open => Some(HoldKind::Queue),
                });
            }
            let checks = fake.checks.lock().unwrap().len();
            fake.clock.fetch_add(60_000, Ordering::SeqCst);
            drop(try_admit_in(&fake.host(), &configured, &run("first")).unwrap());
            let after_expiry = fake.checks.lock().unwrap().len();
            (kinds, checks, after_expiry)
        })
        .collect();
    assert_eq!(
        results,
        [
            (vec![None, None], 2, 3),
            (vec![Some(HoldKind::Capacity); 2], 1, 2),
            (vec![Some(HoldKind::CheckFailed); 2], 1, 2),
            (vec![Some(HoldKind::CheckFailed); 2], 1, 2),
            (vec![Some(HoldKind::CheckFailed); 2], 1, 2),
            (vec![Some(HoldKind::CheckFailed); 2], 1, 2),
        ]
    );
}

#[test]
fn capacity_look_never_writes_a_ticket_and_reuses_only_holds() {
    let fake = FakeHost::new();
    let configured = config("check = 'capacity'");
    *fake.check_answer.lock().unwrap() = Ok((Some(75), false));
    let full = look_in(fake.as_ref(), &configured, &run("first")).unwrap();
    assert!(
        matches!(
            full,
            Some(Hold {
                kind: HoldKind::Capacity,
                ..
            })
        ),
        "{full:?}"
    );
    assert_eq!(fake.tickets(), 0);
    *fake.check_answer.lock().unwrap() = Ok((Some(0), false));
    assert!(
        look_in(fake.as_ref(), &configured, &run("first"))
            .unwrap()
            .is_some()
    );
    fake.clock.fetch_add(60_000, Ordering::SeqCst);
    assert!(
        look_in(fake.as_ref(), &configured, &run("first"))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        try_admit_in(&fake.host(), &configured, &run("first")).unwrap(),
        Attempt::Turn(_)
    ));
    assert_eq!(
        fake.checks.lock().unwrap().len(),
        3,
        "capacity is checked again at admission"
    );
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn capacity_holds_are_cached_per_kind_and_account_and_diagnose_the_latest_error() {
    let fake = FakeHost::new();
    let configured = config("check = 'capacity'");
    *fake.check_answer.lock().unwrap() = Ok((Some(2), false));
    drop(try_admit_in(&fake.host(), &configured, &run("first")).unwrap());
    let errors = check_errors_in(fake.as_ref(), &configured);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("claude") && errors[0].contains("exit 2"),
        "{errors:?}"
    );
    let mut other = run("other");
    other.groups[0].account = Some("work".parse().unwrap());
    *fake.check_answer.lock().unwrap() = Ok((Some(0), false));
    assert!(matches!(
        try_admit_in(&fake.host(), &configured, &other).unwrap(),
        Attempt::Turn(_)
    ));
    assert_eq!(
        check_errors_in(fake.as_ref(), &configured).len(),
        1,
        "another account cannot clear an error"
    );
    fake.clock.fetch_add(60_000, Ordering::SeqCst);
    drop(try_admit_in(&fake.host(), &configured, &run("first")).unwrap());
    assert!(check_errors_in(fake.as_ref(), &configured).is_empty());
}

#[test]
fn capacity_waiters_step_aside_only_for_disjoint_kinds_and_never_a_held_turn() {
    let results: Vec<_> = [HoldKind::Capacity, HoldKind::CheckFailed]
        .into_iter()
        .map(|kind| {
            let fake = FakeHost::new();
            let first = run("first");
            let path = enqueue_as(&fake, 1, &first);
            let mut ticket = read_ticket(&path).unwrap().unwrap();
            ticket.state = TicketState::Waiting {
                reason: Some(Hold {
                    kind,
                    text: "no capacity".into(),
                    kinds: vec![AgentKind::new_unchecked("claude")],
                }),
            };
            write_ticket(&path, &ticket).unwrap();
            let mut other = run("other");
            other.groups[0].kind = AgentKind::new_unchecked("codex");
            let codex = try_admit_in(&fake.host(), &config(""), &other).unwrap();
            let claude = try_admit_in(&fake.host(), &config(""), &run("next")).unwrap();
            let held_turn = try_admit_in(&fake.host(), &config(""), &other).unwrap();
            (
                matches!(codex, Attempt::Turn(_)),
                matches!(claude, Attempt::Held(_)),
                matches!(held_turn, Attempt::Held(_)),
            )
        })
        .collect();
    assert_eq!(results, [(true, true, true); 2]);
}

#[test]
fn capacity_queued_same_kind_waiters_do_not_block_other_kinds() {
    let fake = FakeHost::new();
    let configured = config("check = 'capacity'");
    *fake.check_answer.lock().unwrap() = Ok((Some(75), false));
    let weak = Arc::downgrade(&fake);
    fake.on_sleep(move |fake| {
        let host = weak.upgrade().unwrap().host();
        let second = run("second");
        let second_path = enqueue_as(fake, 2, &second);
        let hold = pass(
            fake,
            &configured,
            &second,
            Some(&second_path),
            fake.now_ms(),
            &mut None,
        )
        .unwrap();
        assert!(hold.is_some(), "the second Claude start must wait");
        let mut codex = run("codex");
        codex.groups[0].kind = AgentKind::new_unchecked("codex");
        let other = try_admit_in(&host, &config(""), &codex).unwrap();
        let third = try_admit_in(&host, &config(""), &run("third")).unwrap();
        assert!(matches!(other, Attempt::Turn(_)), "{other:?}");
        assert!(matches!(third, Attempt::Held(_)), "{third:?}");
        fake.interrupt.store(true, Ordering::SeqCst);
        remove_ticket(&second_path);
    });
    let result = admit(
        &fake.host(),
        &config("check = 'capacity'"),
        &run("first"),
        None,
    )
    .unwrap();
    assert!(matches!(result, Admission::Interrupted), "{result:?}");
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn capacity_check_stops_at_the_give_up_instant_without_caching_an_error() {
    let fake = FakeHost::new();
    let cut_at_the_bound = |fake: &FakeHost| {
        *fake.on_check.lock().unwrap() = Some(Box::new(|fake| {
            let timeout = *fake.check_timeouts.lock().unwrap().last().unwrap();
            fake.clock.fetch_add(duration_ms(timeout), Ordering::SeqCst);
            *fake.check_answer.lock().unwrap() = Ok((None, true));
        }));
    };
    cut_at_the_bound(&fake);
    let run = Run {
        gives_up_ms: Some(fake.now_ms() + 3_000),
        ..run("first")
    };
    let result = admit(&fake.host(), &config("check = 'capacity'"), &run, None).unwrap();
    assert!(matches!(result, Admission::Skipped { .. }), "{result:?}");
    assert_eq!(fake.elapsed_ms(), 3_000);
    assert!(read_verdicts(fake.as_ref()).is_empty());
    assert_eq!(fake.tickets(), 0);
    cut_at_the_bound(&fake);
    let once = Run {
        gives_up_ms: Some(fake.now_ms() + 3_000),
        ..run.clone()
    };
    let attempt = try_admit_in(&fake.host(), &config("check = 'capacity'"), &once).unwrap();
    assert!(matches!(attempt, Attempt::Held(_)), "{attempt:?}");
    assert!(read_verdicts(fake.as_ref()).is_empty());
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn capacity_expired_admission_without_a_check_has_a_neutral_reason() {
    let fake = FakeHost::new();
    let run = Run {
        gives_up_ms: Some(fake.now_ms()),
        ..run("first")
    };
    let result = admit(&fake.host(), &config(""), &run, None).unwrap();
    let Admission::Skipped { reason } = result else {
        panic!("expected a skip, got {result:?}");
    };
    assert_eq!(
        reason,
        "gave up waiting for capacity after 0s; wait deadline reached"
    );
}

#[test]
fn capacity_removed_check_stops_reporting_cached_errors() {
    let fake = FakeHost::new();
    *fake.check_answer.lock().unwrap() = Ok((Some(2), false));
    drop(try_admit_in(&fake.host(), &config("check = 'capacity'"), &run("first")).unwrap());
    assert!(check_errors_in(fake.as_ref(), &config("")).is_empty());
}

#[test]
fn capacity_check_timeout_keeps_its_first_stderr_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = run("first");
    run.root = dir.path().to_owned();
    run.checkout = run.root.clone();
    let configured = config("check = 'echo boom >&2; sleep 5'\ncheck-timeout = '1s'");
    let timeout = configured.check_timeout();
    let checked = SystemHost.check(&configured, &run, &run.groups[0], timeout);
    assert_eq!(
        check_answer(checked, timeout),
        (
            CheckAnswer::Error,
            "capacity check timed out after 1s: boom".to_owned()
        )
    );
}

#[test]
fn capacity_check_bounds_pipe_holders_after_the_leader_exits() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = run("first");
    run.root = dir.path().to_owned();
    run.checkout = run.root.clone();
    let configured = config("check = 'sleep 3 & exit 75'\ncheck-timeout = '1s'");
    let started = std::time::Instant::now();
    let checked = SystemHost
        .check(
            &configured,
            &run,
            &run.groups[0],
            configured.check_timeout(),
        )
        .unwrap();
    assert_eq!(checked.code(), Some(75));
    assert!(!checked.timed_out());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "pipe holders exceeded the timeout"
    );
}

#[test]
fn capacity_check_extends_the_ceiling_but_provider_reports_still_release_it() {
    let results: Vec<_> = [false, true]
        .into_iter()
        .map(|report| {
            let fake = FakeHost::new();
            let configured = config("pace = '1s'\ncheck = 'capacity'");
            let (first, _) = admitted(admit_quiet(&fake, &configured, &run("first")));
            fake.set_agents(&workspace(), vec![launched("launch_a")]);
            first.report_launch(&workspace(), &AgentSessionId::from("launch_a"));
            fake.clock.fetch_add(1_000, Ordering::SeqCst);
            let held = try_admit_in(&fake.host(), &configured, &run("next")).unwrap();
            if report {
                let mut adopted = launched("provider-session");
                adopted.launch_id = Some(AgentSessionId::from("launch_a"));
                fake.set_agents(&workspace(), vec![adopted]);
            } else {
                fake.clock.fetch_add(119_000, Ordering::SeqCst);
            }
            let released = try_admit_in(&fake.host(), &configured, &run("next")).unwrap();
            (
                matches!(held, Attempt::Held(_)),
                matches!(released, Attempt::Turn(_)),
            )
        })
        .collect();
    assert_eq!(results, [(true, true); 2]);
}

#[test]
fn capacity_blocking_admission_uses_the_callers_instant_or_waits_forever() {
    let results: Vec<_> = [Some(3_000), None]
        .into_iter()
        .map(|bound| {
            let fake = FakeHost::new();
            fake.set_pressure(90.0, 90.0);
            fake.on_sleep(|fake| {
                if fake.elapsed_ms() >= 5_000 {
                    fake.set_pressure(0.0, 0.0);
                }
            });
            let run = Run {
                gives_up_ms: bound.map(|ms| fake.now_ms() + ms),
                ..run("first")
            };
            let admission = admit(&fake.host(), &config("cpu-pressure = 25"), &run, None).unwrap();
            (
                matches!(admission, Admission::Skipped { .. }),
                fake.elapsed_ms(),
            )
        })
        .collect();
    assert_eq!(results, [(true, 3_000), (false, 5_000)]);
}

#[test]
fn capacity_blocking_checks_retry_without_holding_the_turn_between_verdicts() {
    let fake = FakeHost::new();
    *fake.check_answer.lock().unwrap() = Ok((Some(75), false));
    fake.on_sleep(|fake| {
        let paths = ticket_paths(&fake.queue_dir()).unwrap();
        let ticket = read_ticket(&paths[0]).unwrap().unwrap();
        assert!(matches!(
            ticket.state,
            TicketState::Waiting {
                reason: Some(Hold {
                    kind: HoldKind::Capacity,
                    ..
                })
            }
        ));
        if fake.elapsed_ms() >= 60_000 {
            *fake.check_answer.lock().unwrap() = Ok((Some(0), false));
        }
    });
    let admission = admit(
        &fake.host(),
        &config("check = 'capacity'"),
        &run("first"),
        None,
    )
    .unwrap();
    let (_turn, waited) = admitted(admission);
    assert_eq!(waited, Some(Duration::from_secs(60)));
    assert_eq!(fake.checked_groups().len(), 2);
}

#[test]
fn capacity_a_slow_check_cannot_admit_after_the_callers_give_up_instant() {
    let results: Vec<_> = [(0, 3_000), (75, 3_000), (0, 4_000), (75, 4_000)]
        .into_iter()
        .map(|(code, elapsed)| {
            let fake = FakeHost::new();
            *fake.check_answer.lock().unwrap() = Ok((Some(code), false));
            *fake.on_check.lock().unwrap() = Some(Box::new(move |fake| {
                fake.clock.fetch_add(elapsed, Ordering::SeqCst);
            }));
            let run = Run {
                gives_up_ms: Some(fake.now_ms() + 3_000),
                ..run("first")
            };
            let admission = admit(&fake.host(), &config("check = 'capacity'"), &run, None).unwrap();
            let Admission::Skipped { reason } = &admission else {
                panic!("expected a skip, got {admission:?}");
            };
            assert!(
                reason.contains(&format!("{}s", elapsed / 1_000)),
                "{reason}"
            );
            (
                matches!(admission, Admission::Skipped { .. }),
                fake.tickets(),
            )
        })
        .collect();
    assert_eq!(results, [(true, 0); 4]);
}

#[test]
fn capacity_only_denied_kinds_hold_and_slow_checks_do_not_extend_cache_freshness() {
    let fake = FakeHost::new();
    let mut run = run("first");
    run.groups.push(StartGroup {
        kind: AgentKind::new_unchecked("codex"),
        account: None,
        agents: 1,
    });
    *fake.on_check.lock().unwrap() = Some(Box::new(|fake| {
        fake.clock.fetch_add(60_000, Ordering::SeqCst);
        *fake.check_answer.lock().unwrap() = Ok((Some(75), false));
    }));
    let configured = config("check = 'capacity'");
    let attempt = try_admit_in(&fake.host(), &configured, &run).unwrap();
    let Attempt::Held(hold) = attempt else {
        panic!("all denied groups must hold: {attempt:?}");
    };
    assert_eq!(
        hold.kinds,
        [
            AgentKind::new_unchecked("claude"),
            AgentKind::new_unchecked("codex")
        ]
    );
    *fake.check_answer.lock().unwrap() = Ok((Some(0), false));
    let attempt = try_admit_in(&fake.host(), &configured, &run).unwrap();
    let Attempt::Held(hold) = attempt else {
        panic!("the second group's cached hold must remain: {attempt:?}");
    };
    assert_eq!(hold.kinds, [AgentKind::new_unchecked("codex")]);
    assert_eq!(
        fake.checked_groups().len(),
        3,
        "the first group's request was already 60s old"
    );
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
    let admission = admit(
        &fake.host(),
        &config("cpu-pressure = 25\n"),
        &run("nightly"),
        None,
    )
    .expect("admit");
    let (_turn, waited) = admitted(admission);
    // Sampled at 0s, 5s, 10s, 15s: the reading cleared at 12s admits at 15s.
    assert_eq!(fake.elapsed_ms(), 15_000);
    assert_eq!(waited, Some(Duration::from_secs(15)));
}

#[test]
fn a_run_held_past_its_give_up_instant_is_skipped_with_its_last_reason() {
    let fake = FakeHost::new();
    fake.set_pressure(90.0, 90.0);
    let run = Run {
        gives_up_ms: Some(fake.now_ms() + 3_000),
        ..run("nightly")
    };
    let skipped = admit(&fake.host(), &config("cpu-pressure = 25"), &run, None).unwrap();
    let Admission::Skipped { reason } = skipped else {
        panic!("expected a skip, got {skipped:?}");
    };
    assert_eq!(
        reason,
        "gave up waiting for capacity after 3s; cpu pressure 90% >= 25%"
    );
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
        let admission = admit(&fake.host(), &pacing, &run("second"), None).expect("admit");
        let (_second, waited) = admitted(admission);
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
    let capped = config("pace = \"0s\"\nmax-active = 1\n");
    fake.set_agents(&workspace(), Vec::new());
    let (first, _) = admitted(admit_quiet(&fake, &capped, &run("first")));
    fake.on_sleep(move |fake| {
        if fake.elapsed_ms() == 2_000 {
            fake.set_agents(&workspace(), vec![launched("launch_a")]);
            first.report_launch(&workspace(), &AgentSessionId::from("launch_a"));
        }
    });
    fake.become_owner(2);
    let run = Run {
        gives_up_ms: Some(fake.now_ms() + 20_000),
        ..run("second")
    };
    let skipped = admit(&fake.host(), &capped, &run, None).expect("admit");
    // Before the report the turn is held; at the report it passes on, and the
    // first sample the second run takes already counts the committed row.
    assert!(
        matches!(skipped, Admission::Skipped { reason } if reason.ends_with("active agents 1 >= 1 (@claude)"))
    );
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
    let held = limit_hold(fake.as_ref(), &capped, &run("nightly"))
        .unwrap()
        .unwrap();
    assert_eq!(held.text, "active agents 2 >= 2 (@otter, @claude)");
    let admission = admit(&fake.host(), &capped, &run("nightly"), None).expect("admit");
    admitted(admission);
    assert_eq!(fake.elapsed_ms(), 10_000, "admitted on the sample after 7s");
    // The explicit sample plus three admission samples (0s, 5s, 10s), two workspaces each.
    assert_eq!(fake.agent_reads.load(Ordering::SeqCst), 8);
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
        kinds: vec![AgentKind::new_unchecked("claude")],
        gives_up_ms: None,
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
            ("cpu pressure", "avg10 2% (limit 60%)".to_owned()),
            ("io pressure", "avg10 2%".to_owned()),
            ("memory pressure", "avg10 2%".to_owned()),
            ("memory", "9 GB available".to_owned()),
            ("disk", "20 GB free".to_owned()),
            ("active agents", "2 (limit max 6)".to_owned()),
            ("task's active agents", "1 (limit max 2)".to_owned()),
        ]
    );
}

#[test]
fn a_held_run_is_skipped_at_its_give_up_instant_even_when_its_hold_clears_then() {
    // The holder's turn passes on exactly at the deadline, or a slow poll
    // first wakes past it: both are a skip, never a late start.
    for (clears_at, delay_ms, waited) in [(3_000, 0, "3s"), (1_000, 5_000, "6s")] {
        let fake = FakeHost::new();
        let paced = config("");
        let mut holder = Some(admitted(admit_quiet(&fake, &paced, &run("first"))).0);
        fake.on_sleep(move |fake| {
            if fake.elapsed_ms() == clears_at {
                fake.clock.fetch_add(delay_ms, Ordering::SeqCst);
                holder.take();
            }
        });
        fake.become_owner(2);
        let run = Run {
            gives_up_ms: Some(fake.now_ms() + 3_000),
            ..run("second")
        };
        let skipped = admit(&fake.host(), &paced, &run, None).unwrap();
        let Admission::Skipped { reason } = skipped else {
            panic!("cleared at {clears_at}ms: expected a skip, got {skipped:?}");
        };
        assert_eq!(
            reason,
            format!("gave up waiting for capacity after {waited}; 1 start ahead")
        );
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
    let error = admit(&fake.host(), &config(""), &run("second"), None)
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
    let pacing = config("");
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
    let interrupted = admit(&fake.host(), &pacing, &run("second"), None).expect("admit");
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
    assert_eq!(fake.elapsed_ms(), 2_000);
    assert_eq!(fake.listening.load(Ordering::SeqCst), 1);
    assert_eq!(fake.tickets(), 1, "only the first run's turn is left");
}

/// A run whose caller already listens is the one held behind `first`.
fn admit_listening(fake: &Arc<FakeHost>, raised: &Arc<AtomicBool>) -> Admission {
    let pacing = config("");
    let (first, _) = admitted(admit_quiet(fake, &pacing, &run("first")));
    fake.become_owner(2);
    let admission = admit(
        &fake.host(),
        &pacing,
        &run("second"),
        Some(Interrupts::raised_by(Arc::clone(raised))),
    )
    .expect("admit");
    drop(first);
    admission
}

#[test]
fn ctrl_c_heard_before_admission_ends_the_run_on_its_first_pass() {
    let fake = FakeHost::new();
    let interrupted = admit_listening(&fake, &Arc::new(AtomicBool::new(true)));
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
    assert_eq!(fake.elapsed_ms(), 0, "no recheck was slept");
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
    let interrupted = admit_listening(&fake, &raised);
    assert!(
        matches!(interrupted, Admission::Interrupted),
        "{interrupted:?}"
    );
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
    let pressure = config("pace = \"0s\"\ncpu-pressure = 50\n");
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
