//! A room in memory for the run's tests: launches, processes, the clock, readings, bookings, and
//! installed workflows, each of which a test can end, fail, kill, or restore, and a crash at any
//! call: the fake panics there, as a process that died at that point stops, and a test catches it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use super::room::{Admit, Ending, Launched, Look, Pin, Room, Seen};
use crate::harness::block::BlockName;
use crate::ids::WorkflowRunId;
use crate::store::workflow_run::Process;

/// Fake processes are numbered from here, past any real process id.
pub(super) const FAKE_PID: u32 = 10_000_000;

#[derive(Clone, Debug)]
pub(super) struct Launch {
    pub pin: Pin,
    pub id: String,
    pub block: BlockName,
    pub object: Value,
    pub handle: Option<String>,
    pub alive: bool,
    pub ending: Option<Ending>,
}

/// Where the fake crashes: the `nth` call of `call`, or of any call, before or after its effect.
struct Crash {
    call: Option<&'static str>,
    nth: usize,
    after: bool,
}

type Hook = Box<dyn FnMut() -> bool>;

/// When a hook runs: at a call before its effect, or after.
#[derive(Clone, Copy, PartialEq, Eq)]
enum When {
    Before,
    After,
}

struct Inner {
    now: Timestamp,
    me: u32,
    next_pid: u32,
    dead: BTreeSet<u32>,
    lingering: BTreeSet<u32>,
    launches: Vec<Launch>,
    boards: BTreeMap<(u64, u32), String>,
    admit: Admit,
    admits: BTreeMap<String, Admit>,
    launch_answer: Option<Launched>,
    hosts: Vec<(Pin, Process)>,
    looks: BTreeMap<String, Look>,
    bookings: Vec<Value>,
    booking_refusal: Option<String>,
    installed: BTreeMap<String, PathBuf>,
    crash: Option<Crash>,
    calls: BTreeMap<&'static str, usize>,
    all_calls: usize,
    stopped: Vec<String>,
    failing: BTreeSet<&'static str>,
    trail: Vec<&'static str>,
    hooks: Vec<(&'static str, When, Hook)>,
}

pub(super) struct Fake {
    inner: RefCell<Inner>,
}

impl Fake {
    pub fn new() -> Self {
        Self {
            inner: RefCell::new(Inner {
                now: "2026-11-28T09:00:00Z".parse().unwrap(),
                me: FAKE_PID,
                next_pid: FAKE_PID + 1,
                dead: BTreeSet::new(),
                lingering: BTreeSet::new(),
                launches: Vec::new(),
                boards: BTreeMap::new(),
                admit: Admit::Go,
                admits: BTreeMap::new(),
                launch_answer: None,
                hosts: Vec::new(),
                looks: BTreeMap::new(),
                bookings: Vec::new(),
                booking_refusal: None,
                installed: BTreeMap::new(),
                crash: None,
                calls: BTreeMap::new(),
                all_calls: 0,
                stopped: Vec::new(),
                failing: BTreeSet::new(),
                trail: Vec::new(),
                hooks: Vec::new(),
            }),
        }
    }

    /// Count a call and crash where the test asked; then run the hooks on it, each until it
    /// answers true.
    fn at(&self, call: &'static str, after: bool) {
        let crash = {
            let mut inner = self.inner.borrow_mut();
            if !after {
                *inner.calls.entry(call).or_default() += 1;
                inner.all_calls += 1;
                inner.trail.push(call);
            }
            let count = match inner.crash.as_ref().and_then(|crash| crash.call) {
                Some(_) => inner.calls.get(call).copied().unwrap_or(0),
                None => inner.all_calls,
            };
            let hit = inner.crash.as_ref().is_some_and(|crash| {
                crash.after == after
                    && crash.nth == count
                    && crash.call.is_none_or(|named| named == call)
            });
            if hit {
                inner.crash = None;
            }
            hit
        };
        if crash {
            panic!("the fake room crashed the process at {call}");
        }
        let when = if after { When::After } else { When::Before };
        let hooks: Vec<_> = {
            let mut inner = self.inner.borrow_mut();
            let (mine, rest) = std::mem::take(&mut inner.hooks)
                .into_iter()
                .partition(|(named, at, _)| *named == call && *at == when);
            inner.hooks = rest;
            mine
        };
        let mut kept = Vec::new();
        for (named, at, mut hook) in hooks {
            if !hook() {
                kept.push((named, at, hook));
            }
        }
        self.inner.borrow_mut().hooks.extend(kept);
    }

    /// Crash at the `nth` call of `call`, before or after its effect.
    pub fn crash_at(&self, call: &'static str, nth: usize, after: bool) {
        self.inner.borrow_mut().crash = Some(Crash {
            call: Some(call),
            nth,
            after,
        });
    }

    /// Crash at the `nth` room call of any kind, counted from now, after its effect when `after`.
    pub fn crash_after_calls(&self, nth: usize, after: bool) {
        let mut inner = self.inner.borrow_mut();
        let nth = inner.all_calls + nth;
        inner.crash = Some(Crash {
            call: None,
            nth,
            after,
        });
    }

    pub fn crash_pending(&self) -> bool {
        self.inner.borrow().crash.is_some()
    }

    pub fn calls(&self, call: &str) -> usize {
        self.inner.borrow().calls.get(call).copied().unwrap_or(0)
    }

    /// Run `hook` at each call of `call`, before its effect, until it answers true.
    pub fn hook(&self, call: &'static str, hook: impl FnMut() -> bool + 'static) {
        let hooks = &mut self.inner.borrow_mut().hooks;
        hooks.push((call, When::Before, Box::new(hook)));
    }

    /// Run `hook` at each call of `call`, after its effect, until it answers true.
    pub fn hook_after(&self, call: &'static str, hook: impl FnMut() -> bool + 'static) {
        let hooks = &mut self.inner.borrow_mut().hooks;
        hooks.push((call, When::After, Box::new(hook)));
    }

    /// Every room call made, by name, in the order made.
    pub fn trail(&self) -> Vec<&'static str> {
        self.inner.borrow().trail.clone()
    }

    pub fn set_now(&self, now: Timestamp) {
        self.inner.borrow_mut().now = now;
    }

    pub fn later(&self, by: jiff::SignedDuration) {
        let mut inner = self.inner.borrow_mut();
        inner.now += by;
    }

    /// A process of the fake's, alive.
    pub fn spawn(&self) -> Process {
        let mut inner = self.inner.borrow_mut();
        let pid = inner.next_pid;
        inner.next_pid += 1;
        Process { pid, start: None }
    }

    pub fn me_is(&self, process: &Process) {
        self.inner.borrow_mut().me = process.pid;
    }

    pub fn kill_process(&self, process: &Process) {
        self.inner.borrow_mut().dead.insert(process.pid);
    }

    /// A kill the room sends `process` has not landed yet: it stays alive.
    pub fn linger(&self, process: &Process) {
        self.inner.borrow_mut().lingering.insert(process.pid);
    }

    /// The process that called the room exits, and a new one takes its place.
    pub fn reborn(&self) {
        let fresh = self.spawn();
        let mut inner = self.inner.borrow_mut();
        let me = inner.me;
        inner.dead.insert(me);
        inner.me = fresh.pid;
    }

    pub fn set_admit(&self, admit: Admit) {
        self.inner.borrow_mut().admit = admit;
    }

    /// Answer `admit` for a call that names `profile`.
    pub fn set_admit_for(&self, profile: &str, admit: Admit) {
        let admits = &mut self.inner.borrow_mut().admits;
        admits.insert(profile.to_owned(), admit);
    }

    pub fn answer_next_launch(&self, launched: Launched) {
        self.inner.borrow_mut().launch_answer = Some(launched);
    }

    pub fn launched(&self) -> Vec<Launch> {
        self.inner.borrow().launches.clone()
    }

    pub fn live(&self) -> Vec<Launch> {
        self.launched()
            .into_iter()
            .filter(|launch| launch.alive)
            .collect()
    }

    pub fn end(&self, id: &str, ending: Option<Ending>) {
        let mut inner = self.inner.borrow_mut();
        if let Some(launch) = inner.launches.iter_mut().find(|launch| launch.id == id) {
            launch.alive = false;
            launch.ending = ending;
        }
    }

    /// A rebirth brought this launch back, alive under the pin it had.
    pub fn restore(&self, pin: Pin, id: &str) {
        let mut inner = self.inner.borrow_mut();
        if let Some(launch) = inner.launches.iter_mut().find(|launch| launch.id == id) {
            launch.alive = true;
            launch.ending = None;
            return;
        }
        inner.launches.push(Launch {
            pin,
            id: id.to_owned(),
            block: BlockName::Agent,
            object: Value::Null,
            handle: Some(format!("restored-{id}")),
            alive: true,
            ending: None,
        });
    }

    pub fn board_done(&self, step: u64, attempt: u32, message: &str) {
        self.inner
            .borrow_mut()
            .boards
            .insert((step, attempt), message.to_owned());
    }

    pub fn take_hosts(&self) -> Vec<(Pin, Process)> {
        std::mem::take(&mut self.inner.borrow_mut().hosts)
    }

    pub fn set_look(&self, when: &str, look: Look) {
        self.inner.borrow_mut().looks.insert(when.to_owned(), look);
    }

    pub fn bookings(&self) -> Vec<Value> {
        self.inner.borrow().bookings.clone()
    }

    pub fn refuse_bookings(&self, reason: &str) {
        self.inner.borrow_mut().booking_refusal = Some(reason.to_owned());
    }

    pub fn install(&self, name: &str, dir: &Path) {
        self.inner
            .borrow_mut()
            .installed
            .insert(name.to_owned(), dir.to_path_buf());
    }

    pub fn stopped(&self) -> Vec<String> {
        self.inner.borrow().stopped.clone()
    }

    /// Fail every call of `call` (`launches` or `stop_launches`) from now on, or no more.
    pub fn fail(&self, call: &'static str, failing: bool) {
        let calls = &mut self.inner.borrow_mut().failing;
        if failing {
            calls.insert(call);
        } else {
            calls.remove(call);
        }
    }

    fn failed(&self, call: &'static str) -> anyhow::Result<()> {
        if self.inner.borrow().failing.contains(call) {
            anyhow::bail!("the fake room failed {call}");
        }
        Ok(())
    }
}

impl Room for Fake {
    fn now(&self) -> Timestamp {
        self.at("now", false);
        self.inner.borrow().now
    }

    fn zone(&self) -> jiff::tz::TimeZone {
        self.at("zone", false);
        let zone = jiff::tz::TimeZone::UTC;
        self.at("zone", true);
        zone
    }

    fn me(&self) -> Process {
        Process {
            pid: self.inner.borrow().me,
            start: None,
        }
    }

    fn alive(&self, process: &Process) -> bool {
        if process.pid < FAKE_PID {
            return crate::proc::process_is_live(process.pid, process.start.as_deref());
        }
        !self.inner.borrow().dead.contains(&process.pid)
    }

    fn admit(&self, _block: BlockName, object: &Value, _checkout: &Path) -> Admit {
        self.at("admit", false);
        let inner = self.inner.borrow();
        let profile = object.get("profile").and_then(Value::as_str);
        profile
            .and_then(|profile| inner.admits.get(profile))
            .unwrap_or(&inner.admit)
            .clone()
    }

    fn launch(&self, block: BlockName, object: &Value, _checkout: &Path, pin: &Pin) -> Launched {
        self.at("launch", false);
        let answer = {
            let mut inner = self.inner.borrow_mut();
            match inner.launch_answer.take() {
                Some(answer @ Launched::Refused(_)) => answer,
                answer => {
                    let seats = if block == BlockName::Team { 2 } else { 1 };
                    let mut ids = Vec::new();
                    for _ in 0..seats {
                        let id = format!("L{}", inner.launches.len() + 1);
                        let handle = (block == BlockName::Agent).then(|| {
                            object
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("agent")
                                .to_owned()
                        });
                        inner.launches.push(Launch {
                            pin: pin.clone(),
                            id: id.clone(),
                            block,
                            object: object.clone(),
                            handle,
                            alive: true,
                            ending: None,
                        });
                        ids.push(id);
                    }
                    let handle = inner
                        .launches
                        .last()
                        .and_then(|launch| launch.handle.clone());
                    answer.unwrap_or(Launched::Started {
                        launches: ids,
                        handle,
                    })
                }
            }
        };
        self.at("launch", true);
        answer
    }

    fn launches(&self, run: &WorkflowRunId) -> anyhow::Result<Vec<Seen>> {
        self.at("launches", false);
        self.failed("launches")?;
        let seen = self
            .inner
            .borrow()
            .launches
            .iter()
            .filter(|launch| launch.pin.run == *run)
            .map(|launch| Seen {
                pin: launch.pin.clone(),
                launch: launch.id.clone(),
                handle: launch.handle.clone(),
                alive: launch.alive,
                ending: launch.ending.clone(),
            })
            .collect();
        Ok(seen)
    }

    fn board(&self, pin: &Pin) -> Option<String> {
        self.at("board", false);
        self.inner
            .borrow()
            .boards
            .get(&(pin.step, pin.attempt))
            .cloned()
    }

    fn stop_launches(&self, launches: &[String]) -> anyhow::Result<()> {
        self.at("stop_launches", false);
        self.failed("stop_launches")?;
        {
            let mut inner = self.inner.borrow_mut();
            for id in launches {
                if let Some(launch) = inner.launches.iter_mut().find(|launch| launch.id == *id)
                    && launch.alive
                {
                    launch.alive = false;
                    launch.ending = Some(Ending::Canceled("stopped by its run".to_owned()));
                    inner.stopped.push(id.clone());
                }
            }
        }
        self.at("stop_launches", true);
        Ok(())
    }

    fn kill(&self, process: &Process, group: bool) -> anyhow::Result<()> {
        self.at("kill", false);
        if process.pid >= FAKE_PID {
            let mut inner = self.inner.borrow_mut();
            if !inner.lingering.contains(&process.pid) {
                inner.dead.insert(process.pid);
            }
        } else if group {
            crate::proc::kill_owned_group(process.pid, process.start.as_deref(), ("", ""));
        } else if crate::proc::process_is_live(process.pid, process.start.as_deref())
            && let Ok(pid) = i32::try_from(process.pid)
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        self.at("kill", true);
        Ok(())
    }

    fn start_host(&self, pin: &Pin) -> anyhow::Result<Process> {
        self.at("start_host", false);
        let host = self.spawn();
        self.inner
            .borrow_mut()
            .hosts
            .push((pin.clone(), host.clone()));
        self.at("start_host", true);
        Ok(host)
    }

    fn look(&self, when: &str, _dir: &Path) -> Look {
        self.at("look", false);
        self.inner
            .borrow()
            .looks
            .get(when)
            .cloned()
            .unwrap_or(Look {
                truth: Some(false),
                readings: BTreeMap::new(),
            })
    }

    fn book(&self, _run: &WorkflowRunId, object: &Value) -> Result<String, String> {
        self.at("book", false);
        let answer = {
            let mut inner = self.inner.borrow_mut();
            match inner.booking_refusal.clone() {
                Some(reason) => Err(reason),
                None => {
                    inner.bookings.push(object.clone());
                    let name = object.get("name").and_then(Value::as_str).unwrap_or("?");
                    Ok(format!("booking-{name}"))
                }
            }
        };
        self.at("book", true);
        answer
    }

    fn installed(&self, name: &str) -> Option<PathBuf> {
        self.inner.borrow().installed.get(name).cloned()
    }
}
