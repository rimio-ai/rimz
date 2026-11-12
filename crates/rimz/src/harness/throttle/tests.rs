use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize};

use jiff::Timestamp;

use super::*;
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};

#[path = "tests/limits.rs"]
mod limits;

type SleepHook = Box<dyn FnMut(&FakeHost) + Send>;

/// A machine the test scripts: a queue in a tempdir, a clock that only
/// `sleep` moves, and readings the sleep hook may change as time passes.
pub(in crate::harness) struct FakeHost {
    dir: tempfile::TempDir,
    clock: AtomicU64,
    owner: Mutex<Owner>,
    dead: Mutex<Vec<u32>>,
    pub(in crate::harness) pressure: Mutex<Result<Pressure, String>>,
    memory: Mutex<Result<u64, String>>,
    disk: Mutex<u64>,
    disk_reads: Mutex<Vec<PathBuf>>,
    rooms: Mutex<BTreeMap<String, Result<Vec<AgentState>, String>>>,
    workflows: Mutex<Vec<crate::store::workflow::RunRecord>>,
    run_errors: Mutex<BTreeMap<String, String>>,
    agent_reads: AtomicUsize,
    on_sleep: Mutex<Option<SleepHook>>,
    on_check: Mutex<Option<SleepHook>>,
    check_answer: Mutex<Result<(Option<i32>, bool), String>>,
    checks: Mutex<Vec<StartGroup>>,
    check_commands: Mutex<Vec<Option<String>>>,
    check_timeouts: Mutex<Vec<Duration>>,
}

impl FakeHost {
    pub(in crate::harness) fn new() -> Arc<Self> {
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
            run_errors: Mutex::new(BTreeMap::new()),
            agent_reads: AtomicUsize::new(0),
            on_sleep: Mutex::new(None),
            on_check: Mutex::new(None),
            check_answer: Mutex::new(Ok((Some(0), false))),
            checks: Mutex::new(Vec::new()),
            check_commands: Mutex::new(Vec::new()),
            check_timeouts: Mutex::new(Vec::new()),
        })
    }

    /// The seam as the gate takes it.
    pub(in crate::harness) fn host(self: &Arc<Self>) -> Arc<dyn Host> {
        Arc::clone(self) as Arc<dyn Host>
    }

    /// Run `hook` after every sleep, with the clock already advanced.
    pub(in crate::harness) fn on_sleep(&self, hook: impl FnMut(&FakeHost) + Send + 'static) {
        *self.on_sleep.lock().unwrap() = Some(Box::new(hook));
    }

    /// Milliseconds since the fake clock started.
    pub(in crate::harness) fn elapsed_ms(&self) -> u64 {
        self.clock.load(Ordering::SeqCst) - 1_000_000
    }

    pub(in crate::harness) fn set_pressure(&self, avg10: f32, avg60: f32) {
        *self.pressure.lock().unwrap() = Ok(Pressure { avg10, avg60 });
    }

    pub(in crate::harness) fn tickets(&self) -> usize {
        ticket_paths(&self.queue_dir()).unwrap().len()
    }

    pub(in crate::harness) fn checked_groups(&self) -> Vec<StartGroup> {
        self.checks.lock().unwrap().clone()
    }

    /// Every path free disk was read at, in order.
    pub(in crate::harness) fn disk_reads(&self) -> Vec<PathBuf> {
        self.disk_reads.lock().unwrap().clone()
    }

    /// Become another process, as a second `rimz loop run` would be.
    pub(in crate::harness) fn become_owner(&self, pid: u32) {
        self.owner.lock().unwrap().pid = pid;
    }

    /// Move the clock and run the sleep hook, as time passing between fires.
    pub(in crate::harness) fn sleep(&self, duration: Duration) {
        self.clock
            .fetch_add(duration_ms(duration), Ordering::SeqCst);
        let hook = self.on_sleep.lock().unwrap().take();
        if let Some(mut hook) = hook {
            hook(self);
            self.on_sleep.lock().unwrap().get_or_insert(hook);
        }
    }

    fn kill(&self, pid: u32) {
        self.dead.lock().unwrap().push(pid);
    }

    pub(in crate::harness) fn set_agents(&self, workspace: &WorkspaceId, agents: Vec<AgentState>) {
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

    fn start_stamps(&self) -> PathBuf {
        self.dir.path().join("start-stamps.json")
    }

    fn check(
        &self,
        config: &ThrottleConfig,
        _run: &Run,
        group: &StartGroup,
        timeout: Duration,
    ) -> Result<crate::harness::check_run::CheckOutcome, String> {
        self.checks.lock().unwrap().push(group.clone());
        self.check_commands
            .lock()
            .unwrap()
            .push(config.check.clone());
        self.check_timeouts.lock().unwrap().push(timeout);
        if let Some(mut hook) = self.on_check.lock().unwrap().take() {
            hook(self);
        }
        self.check_answer
            .lock()
            .unwrap()
            .clone()
            .map(|(code, timed_out)| {
                crate::harness::check_run::CheckOutcome::new(
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
    fn queue_dir(&self) -> PathBuf {
        self.dir.path().join("queue")
    }

    fn queue_lock(&self) -> PathBuf {
        self.dir.path().join("queue.lock")
    }

    fn owner(&self) -> Owner {
        self.owner.lock().unwrap().clone()
    }

    fn owner_is_live(&self, owner: &Owner) -> bool {
        !self.dead.lock().unwrap().contains(&owner.pid)
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
        let mut workspaces: Vec<_> = self
            .rooms
            .lock()
            .unwrap()
            .keys()
            .map(|id| WorkspaceId::parse(id).expect("workspace id"))
            .chain(
                self.workflows
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|run| WorkspaceId::from_project_root(&run.project_root)),
            )
            .collect();
        workspaces.sort_by_key(ToString::to_string);
        workspaces.dedup();
        Ok(workspaces)
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

    fn workflows(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<crate::store::workflow::RunRecord>, String> {
        if let Some(error) = self.run_errors.lock().unwrap().get(&workspace.to_string()) {
            return Err(error.clone());
        }
        Ok(self
            .workflows
            .lock()
            .unwrap()
            .iter()
            .filter(|run| &WorkspaceId::from_project_root(&run.project_root) == workspace)
            .cloned()
            .collect())
    }
}

fn config(text: &str) -> ThrottleConfig {
    toml::from_str(text).expect("throttle config")
}

#[test]
fn ticket_scopes_preserve_legacy_machine_turns_and_round_trip_limited_turns() {
    let legacy = serde_json::json!({
        "owner":{"pid":1,"start":null}, "task":"nightly", "root":"/repo",
        "checkout":"/repo", "enqueued_ms":1, "state":"admitted"
    });
    let mut limited = legacy.clone();
    limited["machine_gated"] = serde_json::json!(false);
    limited["limit"] = serde_json::json!({"workflow":"review", "step":"build"});
    let results: Vec<_> = [legacy, limited]
        .into_iter()
        .map(|value| {
            let ticket: Ticket = serde_json::from_value(value).unwrap();
            let value = serde_json::to_value(ticket).unwrap();
            (value["machine_gated"].clone(), value["limit"].clone())
        })
        .collect();
    assert_eq!(
        results,
        [
            (serde_json::json!(true), serde_json::Value::Null),
            (
                serde_json::json!(false),
                serde_json::json!({"workflow":"review", "step":"build"})
            )
        ]
    );
}

fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(Path::new("/repo"))
}

fn run(task: &str) -> Run {
    Run {
        machine_gated: true,
        limits: None,
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

fn admitted(attempt: Attempt) -> (Turn, Option<Duration>) {
    match attempt {
        Attempt::Turn(turn) => (turn, None),
        other => panic!("expected a turn, got {other:?}"),
    }
}

pub(crate) fn assert_committed_launch_counts_once(launch: impl FnOnce(Turn) -> Vec<AgentState>) {
    let fake = FakeHost::new();
    let configured = config("max-parallel-agents=2");
    let mut candidate = run("first");
    candidate.machine_gated = false;
    let (turn, _) = admitted(admit_quiet(&fake, &configured, &candidate));
    let rows = launch(turn.clone());
    assert_eq!(rows.len(), 1, "the launch batch committed");
    assert!(open(&rows[0]), "the committed row still occupies a slot");
    fake.set_agents(&candidate.workspace, rows);
    let next = try_admit_in(&fake.host(), &configured, &candidate).unwrap();
    assert!(
        matches!(next, Attempt::Turn(_)),
        "one slot is still free: {next:?}"
    );
}

fn admit_quiet(fake: &Arc<FakeHost>, config: &ThrottleConfig, run: &Run) -> Attempt {
    let run = Run {
        gives_up_ms: run.gives_up_ms.or(Some(fake.now_ms() + 1_800_000)),
        ..run.clone()
    };
    try_admit_in(&fake.host(), config, &run).expect("admit")
}

/// One pass for a ticket that is not this thread's, as its owner would run it.
fn step(fake: &FakeHost, config: &ThrottleConfig, run: &Run, ticket: &Path) -> Option<String> {
    pass(fake, config, run, Some(ticket), fake.now_ms(), &mut None)
        .expect("pass")
        .hold()
        .map(|hold| hold.text)
}

fn enqueue_as(fake: &FakeHost, pid: u32, run: &Run) -> PathBuf {
    fake.become_owner(pid);
    fake.clock.fetch_add(1, Ordering::SeqCst);
    let path = paths::loop_throttle_ticket_in(&fake.queue_dir(), fake.now_ms());
    write_ticket(&path, &Ticket::new(fake, run, fake.now_ms())).expect("enqueue");
    path
}

#[test]
fn nothing_configured_writes_no_ticket() {
    let fake = FakeHost::new();
    let mut candidate = run("nightly");
    candidate.machine_gated = false;
    let open = admit_quiet(&fake, &config(""), &candidate);
    assert!(matches!(open, Attempt::Open), "{open:?}");
    assert_eq!(fake.tickets(), 0);
}

#[test]
fn automation_counts_inherited_children_but_not_user_agents() {
    let fake = FakeHost::new();
    let mut parent = agent("parent", AgentStatus::Idle);
    parent.automation = Some(crate::agents::Automation::Loop);
    let mut child = parent.clone();
    child.agent_id = "child".into();
    child.name = Some("child".into());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.launch_depth = Some(1);
    fake.set_agents(
        &workspace(),
        vec![parent, child, agent("user", AgentStatus::Waiting)],
    );
    let hold = limit_hold(
        fake.as_ref(),
        &config("max-parallel-automation=2"),
        &run("next"),
        0,
    )
    .unwrap();
    assert_eq!(
        hold.map(|hold| hold.text),
        Some("2 of 2 automated agents open (@parent, @child)".into())
    );
    let hold = limit_hold(
        fake.as_ref(),
        &config("max-parallel-agents=3"),
        &run("next"),
        0,
    )
    .unwrap();
    assert_eq!(
        hold.map(|hold| hold.text),
        Some("3 of 3 agents open (@parent, @child, +1)".into())
    );
}

#[test]
fn off_starts_reserve_the_last_automation_slot_under_the_queue_lock() {
    for (agents, cap) in [(1, 1), (2, 2), (2, 3)] {
        let fake = FakeHost::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let threads: Vec<_> = ["first", "second"]
                .into_iter()
                .map(|name| {
                    let fake = Arc::clone(&fake);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        let mut candidate = run(name);
                        candidate.machine_gated = false;
                        candidate.groups[0].agents = agents;
                        barrier.wait();
                        try_admit_in(
                            &fake.host(),
                            &config(&format!("max-parallel-automation={cap}")),
                            &candidate,
                        )
                        .unwrap()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Attempt::Turn(_)))
                .count(),
            1,
            "{results:?}"
        );
        let group = if agents > 1 {
            format!("; this start opens {agents}")
        } else {
            String::new()
        };
        let expected = format!("0 of {cap} automated agents open, {agents} more starting{group}");
        let held = results
            .iter()
            .filter(|result| match result {
                Attempt::Held(hold) => hold.text == expected,
                _ => false,
            })
            .count();
        assert_eq!(held, 1, "{results:?}");
    }
}

#[test]
fn oversized_groups_refuse_machine_admission() {
    for key in ["max-parallel-agents", "max-parallel-automation"] {
        for machine_gated in [true, false] {
            let fake = FakeHost::new();
            let configured = config(&format!("{key}=2"));
            let mut candidate = run("oversized");
            candidate.machine_gated = machine_gated;
            candidate.groups[0].agents = 3;
            candidate.gives_up_ms = Some(fake.now_ms() + 1_000);
            let expected = format!(
                "this start opens 3 agents, more than {key} = 2 allows; raise it in ~/.rimz/config.toml"
            );
            let one_shot = try_admit_in(&fake.host(), &configured, &candidate);
            assert!(one_shot.is_err(), "{key}: {one_shot:?}");
            assert_eq!(one_shot.unwrap_err().to_string(), expected);
            assert_eq!(fake.elapsed_ms(), 0, "an impossible start never waits");
            assert_eq!(fake.tickets(), 0);
        }
    }
}

#[test]
fn cap_holds_name_the_incoming_group_separately_from_open_agents() {
    for (key, label) in [
        ("max-parallel-agents", ""),
        ("max-parallel-automation", "automated "),
    ] {
        let fake = FakeHost::new();
        let mut opened = [agent("a", AgentStatus::Idle), agent("b", AgentStatus::Idle)];
        for agent in &mut opened {
            agent.automation = Some(crate::agents::Automation::Loop);
        }
        fake.set_agents(&workspace(), opened.into());
        let mut candidate = run("pair");
        candidate.groups[0].agents = 2;
        let hold = limit_hold(fake.as_ref(), &config(&format!("{key}=3")), &candidate, 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            hold.text,
            format!("2 of 3 {label}agents open (@a, @b); this start opens 2")
        );
    }
}

#[test]
fn machine_stagger_is_stamped_only_by_gated_reports_and_releases_at_the_exact_gap() {
    let fake = FakeHost::new();
    let configured = config("stagger='30s'\nmax-parallel-agents=10");
    let Attempt::Turn(turn) = try_admit_in(&fake.host(), &configured, &run("first")).unwrap()
    else {
        panic!("a gated start takes a turn");
    };
    assert!(!fake.start_stamps().exists());
    let mut adopted = launched("first");
    adopted.agent_id = "provider-first".into();
    adopted.status = AgentStatus::Idle;
    fake.set_agents(&workspace(), vec![adopted]);
    turn.report_launch(&workspace(), &"first".into());
    let held = try_admit_in(&fake.host(), &configured, &run("next")).unwrap();
    assert!(matches!(held, Attempt::Held(hold) if hold.text == "next start in 30s"));
    fake.clock.fetch_add(29_999, Ordering::SeqCst);
    let mut off = run("off");
    off.machine_gated = false;
    let Attempt::Turn(off_turn) = try_admit_in(&fake.host(), &configured, &off).unwrap() else {
        panic!("an off start reserves cap slots without taking the machine turn");
    };
    off_turn.report_launch(&workspace(), &"off".into());
    let held = try_admit_in(&fake.host(), &configured, &run("next")).unwrap();
    assert!(matches!(held, Attempt::Held(hold) if hold.text == "next start in 1s"));
    fake.clock.fetch_add(1, Ordering::SeqCst);
    assert!(matches!(
        try_admit_in(&fake.host(), &configured, &run("next")).unwrap(),
        Attempt::Turn(_)
    ));
}

#[test]
fn capacity_nonblocking_holds_leave_no_ticket_and_do_not_check_behind_limits() {
    let results: Vec<_> = ["queue", "limit", "full", "error"]
        .into_iter()
        .map(|case| {
            let fake = FakeHost::new();
            let mut configured = config("check = 'capacity'\n");
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
            matches!(&second, Attempt::Held(hold) if hold.text == "queued: 1 start ahead"),
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
fn capacity_checks_refresh_open_counts_after_running_off_lock() {
    let configured = config("check='capacity'\nmax-parallel-agents=1");
    {
        let fake = FakeHost::new();
        *fake.on_check.lock().unwrap() = Some(Box::new(|fake| {
            fake.set_agents(&workspace(), vec![agent("new", AgentStatus::Idle)]);
        }));
        let mut candidate = run("nightly");
        candidate.gives_up_ms = Some(fake.now_ms() + 1_000);
        let expected = "1 of 1 agents open (@claude)";
        let attempt = try_admit_in(&fake.host(), &configured, &candidate).unwrap();
        assert!(
            matches!(&attempt, Attempt::Held(hold) if hold.text == expected),
            "{attempt:?}"
        );
        assert_eq!(fake.tickets(), 0);
    }
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
    let reasons = [
        None,
        Some("claude: no capacity (your check)"),
        Some("capacity check failing: claude exit 2"),
        Some("capacity check failing: claude timed out after 30s"),
        Some("capacity check failing: claude terminated by signal"),
        Some("capacity check failing: claude cannot spawn"),
    ];
    let results: Vec<_> = answers
        .into_iter()
        .zip(reasons)
        .map(|(answer, reason)| {
            let fake = FakeHost::new();
            *fake.check_answer.lock().unwrap() = answer;
            let configured = config("check = 'capacity'");
            let mut kinds = Vec::new();
            for _ in 0..2 {
                let attempt = try_admit_in(&fake.host(), &configured, &run("first")).unwrap();
                kinds.push(match attempt {
                    Attempt::Turn(_) => None,
                    Attempt::Held(hold) => {
                        assert_eq!(Some(hold.text.as_str()), reason);
                        Some(hold.kind)
                    }
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
fn queued_starts_keep_the_blocking_cause_and_its_text() {
    let fake = FakeHost::new();
    let capacity = (HoldKind::Capacity, "claude: no capacity (your check)");
    let failure = (
        HoldKind::CheckFailed,
        "capacity check failing: claude exit 2",
    );
    let another_failure = (
        HoldKind::CheckFailed,
        "capacity check failing: claude timed out after 30s",
    );
    let queued = (HoldKind::Queue, "queued: 1 start ahead");
    let mine = Ticket::new(fake.as_ref(), &run("last"), fake.now_ms());
    let mut actual = Vec::new();
    for blockers in [
        vec![capacity, capacity],
        vec![capacity, failure, another_failure],
        vec![failure, queued],
        vec![queued],
    ] {
        let queue = blockers
            .iter()
            .map(|(kind, text)| {
                let mut ticket = Ticket::new(fake.as_ref(), &run("first"), fake.now_ms());
                ticket.state = TicketState::Waiting {
                    reason: Some(Hold {
                        kind: *kind,
                        text: (*text).into(),
                        kinds: mine.kinds.clone(),
                    }),
                };
                (PathBuf::new(), ticket)
            })
            .collect::<Vec<_>>();
        let hold = queue_hold(&queue, queue.len(), &mine).unwrap();
        actual.push((hold.kind, hold.text));
    }
    assert_eq!(
        actual,
        [
            (capacity.0, capacity.1.into()),
            (failure.0, failure.1.into()),
            (failure.0, failure.1.into()),
            (HoldKind::Queue, "queued: 1 start ahead".into()),
        ]
    );
}

#[test]
fn queued_needs_fix_causes_survive_mixed_predecessors_and_held_turns() {
    let fake = FakeHost::new();
    let disk = (HoldKind::Disk, "free disk 1 GB < 20 GB at /other-room");
    let store = (HoldKind::Store, "cannot count open agents: broken room");
    let failure = (
        HoldKind::CheckFailed,
        "capacity check failing: claude exit 2",
    );
    let queued = (HoldKind::Queue, "queued: 1 start ahead");
    let mine = Ticket::new(fake.as_ref(), &run("last"), fake.now_ms());
    for held_turn in [false, true] {
        for (blockers, expected) in [
            (vec![disk], disk),
            (vec![store], store),
            (vec![queued, disk], disk),
            (vec![disk, queued], disk),
            (vec![disk, store], disk),
            (vec![store, disk], store),
            (vec![queued, failure], failure),
        ] {
            let mut queue = Vec::new();
            if held_turn {
                let mut ticket = mine.clone();
                ticket.state = TicketState::Admitted;
                queue.push((PathBuf::new(), ticket));
            }
            for (kind, text) in blockers {
                let mut ticket = Ticket::new(fake.as_ref(), &run("first"), fake.now_ms());
                ticket.state = TicketState::Waiting {
                    reason: Some(Hold {
                        kind,
                        text: text.into(),
                        kinds: mine.kinds.clone(),
                    }),
                };
                queue.push((PathBuf::new(), ticket));
            }
            let hold = queue_hold(&queue, queue.len(), &mine).unwrap();
            assert_eq!((hold.kind, hold.text.as_str()), expected);
            assert!(hold.kinds.is_empty(), "a mixed queue cannot step aside");
        }
    }
}

#[test]
fn disjoint_starts_cannot_pass_a_propagated_needs_fix_follower() {
    for kind in [HoldKind::Disk, HoldKind::Store, HoldKind::CheckFailed] {
        let fake = FakeHost::new();
        let head = enqueue_as(&fake, 1, &run("head"));
        let mut ticket = read_ticket(&head).unwrap().unwrap();
        ticket.state = TicketState::Waiting {
            reason: Some(Hold {
                kind,
                text: "the head's blocker".into(),
                kinds: ticket.kinds.clone(),
            }),
        };
        write_ticket(&head, &ticket).unwrap();
        let mixed = enqueue_as(&fake, 2, &run("mixed"));
        let follower = enqueue_as(&fake, 3, &run("follower"));
        step(&fake, &config(""), &run("follower"), &follower);
        let TicketState::Waiting {
            reason: Some(propagated),
        } = read_ticket(&follower).unwrap().unwrap().state
        else {
            panic!("the follower must wait on the head's blocker");
        };
        assert!(
            propagated.kinds.is_empty(),
            "a mixed queue cannot step aside"
        );
        remove_ticket(&head);
        remove_ticket(&mixed);
        let mut codex = run("codex");
        codex.groups[0].kind = AgentKind::new_unchecked("codex");
        let attempt = try_admit_in(&fake.host(), &config(""), &codex).unwrap();
        let Attempt::Held(hold) = attempt else {
            panic!("a propagated mixed hold must block disjoint starts: {attempt:?}");
        };
        assert_eq!(hold.kind, kind);
        assert_eq!(hold.text, "the head's blocker");
        assert_eq!(
            hold.kinds,
            codex.kinds(),
            "the record names this run's providers"
        );
    }
}

#[test]
fn failing_checks_lead_mixed_capacity_reasons_and_the_doctor_cache() {
    let fake = FakeHost::new();
    let configured = config("check='capacity'");
    let mut candidate = run("first");
    *fake.check_answer.lock().unwrap() = Ok((Some(75), false));
    check_groups(fake.as_ref(), &configured, &candidate).unwrap();
    candidate.groups.push(StartGroup {
        kind: AgentKind::new_unchecked("codex"),
        account: Some("work".parse().unwrap()),
        agents: 1,
    });
    *fake.check_answer.lock().unwrap() = Ok((Some(2), false));
    let held = check_groups(fake.as_ref(), &configured, &candidate)
        .unwrap()
        .unwrap();
    assert_eq!(held.kind, HoldKind::CheckFailed);
    assert_eq!(
        held.text,
        "capacity check failing: codex/work exit 2; claude: no capacity (your check)"
    );
    assert_eq!(
        check_errors_in(fake.as_ref(), &configured),
        ["capacity check failing: codex/work exit 2"]
    );
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
    let attempt = try_admit_in(&fake.host(), &config("check = 'capacity'"), &run).unwrap();
    assert!(matches!(attempt, Attempt::Held(_)), "{attempt:?}");
    assert!(read_verdicts(fake.as_ref()).is_empty());
    assert_eq!(fake.tickets(), 0);
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
        (CheckAnswer::Error, "timed out after 1s: boom".to_owned())
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
            let configured = config("check = 'capacity'");
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
    let limited = config("cpu-pressure = 25\n");
    // The long average alone, then the short average alone, over the limit still holds.
    for (avg10, avg60, admits) in [(10.0, 41.0, false), (30.0, 12.0, false), (24.0, 12.0, true)] {
        fake.set_pressure(avg10, avg60);
        let attempt = try_admit_in(&fake.host(), &limited, &run("nightly")).unwrap();
        assert_eq!(
            matches!(attempt, Attempt::Turn(_)),
            admits,
            "{avg10}/{avg60}: {attempt:?}"
        );
    }
}

#[test]
fn the_next_start_waits_for_the_report_then_the_provider_or_the_ceiling() {
    for (observed_at, admitted_at) in [(Some(5_000), 5_000), (None, 13_000)] {
        let fake = FakeHost::new();
        let configured = config("");
        let (first, _) = admitted(admit_quiet(&fake, &configured, &run("first")));

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
        let _second = loop {
            match try_admit_in(&fake.host(), &configured, &run("second")).unwrap() {
                Attempt::Turn(turn) => break turn,
                Attempt::Held(_) => fake.sleep(Duration::from_secs(1)),
                Attempt::Open => panic!("the queue gates every machine start"),
            }
        };
        assert_eq!(
            fake.elapsed_ms(),
            admitted_at,
            "observed at {observed_at:?}"
        );
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
fn cap_only_off_starts_count_the_committed_row_at_the_report() {
    let fake = FakeHost::new();
    let off_run = |task| Run {
        machine_gated: false,
        ..run(task)
    };
    let capped = config("max-parallel-agents = 1\n");
    fake.set_agents(&workspace(), Vec::new());
    let (first, _) = admitted(admit_quiet(&fake, &capped, &off_run("first")));
    fake.become_owner(2);
    // Before the report the admitted start reserves a slot; afterward its row does.
    for (report, expected) in [
        (false, "0 of 1 agents open, 1 more starting"),
        (true, "1 of 1 agents open (@claude)"),
    ] {
        if report {
            fake.set_agents(&workspace(), vec![launched("launch_a")]);
            first.report_launch(&workspace(), &AgentSessionId::from("launch_a"));
        }
        let held = try_admit_in(&fake.host(), &capped, &off_run("second")).unwrap();
        assert!(
            matches!(&held, Attempt::Held(hold) if hold.text == expected),
            "reported {report}: {held:?}"
        );
    }
}

#[test]
fn a_dropped_turn_and_a_dead_owner_both_release_the_next_run() {
    let fake = FakeHost::new();
    let configured = config("");
    let (dropped, _) = admitted(admit_quiet(&fake, &configured, &run("first")));
    drop(dropped);
    assert_eq!(fake.tickets(), 0, "an unreported turn removes its ticket");

    fake.become_owner(2);
    let (orphan, _) = admitted(admit_quiet(&fake, &configured, &run("second")));
    // The owner dies holding the turn: nothing runs its drop.
    std::mem::forget(orphan);
    fake.become_owner(3);
    let waiting = enqueue_as(&fake, 3, &run("third"));
    assert_eq!(
        step(&fake, &configured, &run("third"), &waiting).as_deref(),
        Some("queued: 1 start ahead")
    );
    fake.kill(2);
    assert_eq!(step(&fake, &configured, &run("third"), &waiting), None);
    assert_eq!(fake.tickets(), 1);
}

#[test]
fn the_machine_cap_counts_open_agents_and_samples_every_five_seconds() {
    let fake = FakeHost::new();
    let capped = config("max-parallel-agents = 6\n");
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
    let mut provider_child = agent("provider-child", AgentStatus::Running);
    provider_child.parent_agent_id = Some("otter".into());
    fake.set_agents(
        &workspace(),
        vec![
            agent("otter", AgentStatus::Running),
            agent("asking", AgentStatus::Waiting),
            agent("resting", AgentStatus::Idle),
            agent("done", AgentStatus::Success),
            ended,
            dead,
            agent("launch_failed", AgentStatus::Failed),
            agent("errored-turn", AgentStatus::Failed),
            provider_child,
        ],
    );
    fake.set_agents(&other, vec![agent("fox", AgentStatus::Running)]);
    let held = limit_hold(fake.as_ref(), &capped, &run("nightly"), 0).unwrap();
    assert!(
        held.is_some(),
        "a live errored-turn agent keeps the machine cap full"
    );
    let held = held.unwrap();
    assert_eq!(held.text, "6 of 6 agents open (@otter, @asking, +4)");
    let mut automated_error = agent("errored-turn", AgentStatus::Failed);
    automated_error.automation = Some(crate::agents::Automation::Loop);
    fake.set_agents(&other, vec![automated_error]);
    assert_eq!(
        limit_hold(
            fake.as_ref(),
            &config("max-parallel-automation=1"),
            &run("nightly"),
            0,
        )
        .unwrap()
        .expect("an errored live automated agent keeps its slot")
        .kind,
        HoldKind::AutomationCap,
    );
    fake.set_agents(&other, Vec::new());
    admitted(try_admit_in(&fake.host(), &capped, &run("nightly")).unwrap());
    // The explicit machine/automation samples plus one admission sample, two workspaces each.
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
    let hold = limit_hold(
        fake.as_ref(),
        &config("max-parallel-agents = 3\n"),
        &run("nightly"),
        0,
    )
    .expect("sample")
    .expect("held");
    assert_eq!(hold.text, "4 of 3 agents open (@a, @b, +2)");
}

#[test]
fn an_unreadable_workspace_holds_the_run_and_names_it() {
    let fake = FakeHost::new();
    fake.rooms.lock().unwrap().insert(
        workspace().to_string(),
        Err("workspace ws_broken unreadable: torn log".to_owned()),
    );
    let hold = limit_hold(
        fake.as_ref(),
        &config("max-parallel-agents = 3\n"),
        &run("nightly"),
        0,
    )
    .expect("sample")
    .expect("held, never a short count");
    assert_eq!(
        hold.text,
        "cannot count open agents: workspace ws_broken unreadable: torn log"
    );
}

#[test]
fn queue_order_follows_enqueue_order_across_owners() {
    let fake = FakeHost::new();
    let configured = config("");
    let tickets = [1, 2, 3].map(|pid| enqueue_as(&fake, pid, &run(&format!("task-{pid}"))));
    let step_for = |index: usize| {
        step(
            &fake,
            &configured,
            &run(&format!("task-{}", index + 1)),
            &tickets[index],
        )
    };
    assert_eq!(step_for(2).as_deref(), Some("queued: 2 starts ahead"));
    assert_eq!(step_for(1).as_deref(), Some("queued: 1 start ahead"));
    assert_eq!(step_for(0), None);
    assert_eq!(
        step_for(1).as_deref(),
        Some("queued: 1 start ahead"),
        "the turn is held"
    );
    remove_ticket(&tickets[0]);
    assert_eq!(step_for(2).as_deref(), Some("queued: 1 start ahead"));
    assert_eq!(step_for(1), None);
}

#[test]
fn a_waiting_run_is_reported_with_its_reason_and_place() {
    let fake = FakeHost::new();
    let configured = config("");
    let (_turn, _) = admitted(admit_quiet(&fake, &configured, &run("first")));
    let waiting = enqueue_as(&fake, 2, &run("second"));
    let since_ms = fake.now_ms();
    let held = |reason: Option<&str>, pid, position| Held {
        pid,
        checkout: PathBuf::from("/repo"),
        since_ms,
        kinds: vec![AgentKind::new_unchecked("claude")],
        gives_up_ms: None,
        reason: reason.map(str::to_owned),
        hold: reason.map(|_| crate::ids::HoldKind::Queue),
        position,
    };
    assert_eq!(
        held_in(fake.as_ref(), "second", Path::new("/repo")),
        [held(None, 2, 2)]
    );
    step(&fake, &configured, &run("second"), &waiting);
    // Every waiting run of the task is listed, each with its own process.
    let mut later = held(None, 3, 3);
    enqueue_as(&fake, 3, &run("second"));
    later.since_ms = fake.now_ms();
    assert_eq!(
        held_in(fake.as_ref(), "second", Path::new("/repo")),
        [held(Some("queued: 1 start ahead"), 2, 2), later.clone()]
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
    assert_eq!(
        unreadable(fake.as_ref(), &config("max-parallel-agents = 2\n")),
        None
    );
    let problem = unreadable(fake.as_ref(), &config("io-pressure = 40\n")).expect("refused");
    assert!(problem.contains("`throttle.io-pressure`"), "{problem}");
    assert!(problem.contains("/proc/pressure/io"), "{problem}");
    assert!(problem.contains("psi=1"), "{problem}");
    let error = try_admit_in(&fake.host(), &config("io-pressure = 40\n"), &run("nightly"))
        .expect_err("a gated fire errors");
    assert!(error.to_string().contains("`throttle.io-pressure`"));
    assert_eq!(fake.tickets(), 0);

    *fake.memory.lock().unwrap() = Err("/proc/meminfo: not found".to_owned());
    let problem = unreadable(fake.as_ref(), &config("min-memory = \"8GB\"\n")).expect("refused");
    assert!(problem.contains("`throttle.min-memory`"), "{problem}");
}

#[test]
fn diagnostic_holds_read_distinct_write_paths_only_with_a_disk_floor() {
    let fake = FakeHost::new();
    *fake.disk.lock().unwrap() = 12_000_000_000;
    let mut machine = MachineConfig {
        throttle: config("min-disk = '20GB'\ncheck = 'exit 2'\n"),
        ..Default::default()
    };
    let entry: TaskEntry = toml::from_str("root = '/repo'\n").unwrap();
    let tasks = [("first", &entry), ("second", &entry)];
    assert_eq!(
        diagnostic_holds_in(fake.as_ref(), &machine, &tasks),
        [DiagnosticHold {
            hold: HoldKind::Disk,
            text: "free disk 12 GB < 20 GB at /repo".into(),
            disk_path: Some("/repo".into()),
        }]
    );
    assert_eq!(fake.disk_reads(), [PathBuf::from("/repo")]);
    *fake.disk.lock().unwrap() = 20_000_000_000;
    assert!(diagnostic_holds_in(fake.as_ref(), &machine, &tasks).is_empty());
    machine.throttle.min_disk = None;
    fake.disk_reads.lock().unwrap().clear();
    assert!(diagnostic_holds_in(fake.as_ref(), &machine, &tasks).is_empty());
    assert!(fake.disk_reads().is_empty());
    assert_eq!(fake.tickets(), 0);
    assert!(!fake.queue_lock().exists());
    assert!(fake.checked_groups().is_empty());
    assert!(!fake.check_cache().exists());
}

#[test]
fn diagnostic_agent_count_failures_require_a_configured_cap() {
    let fake = FakeHost::new();
    fake.rooms.lock().unwrap().insert(
        workspace().to_string(),
        Err("workspace ws_broken unreadable: torn log".into()),
    );
    let entry: TaskEntry = toml::from_str("root = '/repo'\n").unwrap();
    let mut machine = MachineConfig::default();
    assert!(diagnostic_holds_in(fake.as_ref(), &machine, &[("nightly", &entry)]).is_empty());
    assert_eq!(fake.agent_reads.load(Ordering::SeqCst), 0);
    for caps in ["max-parallel-agents = 2", "max-parallel-automation = 1"] {
        machine.throttle = config(caps);
        assert_eq!(
            diagnostic_holds_in(fake.as_ref(), &machine, &[("nightly", &entry)]),
            [DiagnosticHold {
                hold: HoldKind::Store,
                text: "cannot count open agents: workspace ws_broken unreadable: torn log".into(),
                disk_path: None,
            }]
        );
    }
    fake.set_agents(&workspace(), Vec::new());
    assert!(diagnostic_holds_in(fake.as_ref(), &machine, &[("nightly", &entry)]).is_empty());
    assert_eq!(fake.tickets(), 0);
    assert!(!fake.queue_lock().exists());
    assert!(fake.checked_groups().is_empty());
}

#[test]
fn memory_and_disk_floors_hold_below_the_floor_and_show_beside_the_reading() {
    let fake = FakeHost::new();
    let floors = config("min-memory = \"8GB\"\nmin-disk = \"20GB\"\n");
    *fake.memory.lock().unwrap() = Ok(3_000_000_000);
    let hold = limit_hold(fake.as_ref(), &floors, &run("nightly"), 0).unwrap();
    assert_eq!(hold.unwrap().text, "available memory 3 GB < 8 GB");
    *fake.memory.lock().unwrap() = Ok(9_000_000_000);
    *fake.disk.lock().unwrap() = 12_000_000_000;
    let hold = limit_hold(fake.as_ref(), &floors, &run("nightly"), 0).unwrap();
    assert_eq!(hold.unwrap().text, "free disk 12 GB < 20 GB at /repo");
    *fake.disk.lock().unwrap() = 20_000_000_000;
    assert_eq!(
        limit_hold(fake.as_ref(), &floors, &run("nightly"), 0).unwrap(),
        None
    );

    fake.set_pressure(2.4, 3.6);
    let mut mine = agent("fixer-1", AgentStatus::Running);
    mine.automation = Some(crate::agents::Automation::Loop);
    fake.set_agents(
        &workspace(),
        vec![agent("otter", AgentStatus::Running), mine],
    );
    let rows = readings_in(
        fake.as_ref(),
        &config("cpu-pressure = 60\nmax-parallel-agents = 6\nmax-parallel-automation = 2\n"),
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
            ("open agents", "2 (limit max 6)".to_owned()),
            ("automated agents", "1 (limit max 2)".to_owned()),
        ]
    );
}

#[test]
fn a_compacting_agent_counts_toward_both_caps_throughout_its_window() {
    for capped in ["max-parallel-agents = 1\n", "max-parallel-automation = 1\n"] {
        let fake = FakeHost::new();
        let now = Timestamp::from_millisecond(i64::try_from(fake.now_ms()).unwrap()).unwrap();
        let compacting = |ago_secs: i64| AgentState {
            automation: Some(crate::agents::Automation::Loop),
            compacting_since: Some(now - jiff::SignedDuration::from_secs(ago_secs)),
            ..agent("otter", AgentStatus::Running)
        };
        fake.set_agents(&workspace(), vec![compacting(10)]);
        assert!(
            limit_hold(fake.as_ref(), &config(capped), &run("nightly"), 0)
                .unwrap()
                .is_some(),
            "{capped}"
        );
        fake.set_agents(
            &workspace(),
            vec![compacting(crate::agents::COMPACTING_WINDOW_SECS + 1)],
        );
        let hold = limit_hold(fake.as_ref(), &config(capped), &run("nightly"), 0)
            .unwrap()
            .unwrap_or_else(|| panic!("{capped}: a stale compaction counts"));
        assert!(
            hold.text.ends_with("agents open (@claude)"),
            "{}",
            hold.text
        );
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
    let error = try_admit_in(&fake.host(), &config(""), &run("second"))
        .expect_err("an unreadable ticket may be a held turn");
    assert!(matches!(error, ThrottleError::Io { path, .. } if path == unreadable));
    assert_eq!(fake.tickets(), 1, "only the unreadable ticket is left");
}
