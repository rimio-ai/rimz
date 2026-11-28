//! The run engine against the fake room over a real state directory.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Value, json};

use super::fake::Fake;
use super::room::{Ending as LaunchEnding, Room};
use super::*;
use crate::harness::block::BlockName;
use crate::store::workflow_run::{AnswerLine, Ending, Event, Origin, Record, Settled, Work};

mod blocks;
mod bounds;
mod command;
mod fixtures;
mod kills;
mod races;
mod resume;

pub(super) const META: &str =
    "export const meta = { name: 'run', description: 'A run under test' }\n";

pub(super) fn script(body: &str) -> Origin {
    Origin::Script(format!("{META}{body}"))
}

pub(super) struct Harness {
    dir: tempfile::TempDir,
    pub state: StatePaths,
    pub checkout: PathBuf,
    pub fake: Rc<Fake>,
    pub room: Rc<dyn Room>,
    pub id: WorkflowRunId,
}

impl Harness {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state =
            StatePaths::for_project_root_under(&dir.path().join("project"), dir.path()).unwrap();
        std::fs::create_dir_all(state.workspace_lock.parent().unwrap()).unwrap();
        std::fs::create_dir_all(state.workflow_runs_dir()).unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let fake = Rc::new(Fake::new());
        let room: Rc<dyn Room> = fake.clone();
        Self {
            dir,
            state,
            checkout,
            fake,
            room,
            id: WorkflowRunId::new(),
        }
    }

    /// A directory of the test's own, `workflow.js` written in it.
    pub fn workflow(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.dir.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workflow.js"), format!("{META}{body}")).unwrap();
        dir
    }

    pub fn input(&self, origin: Origin) -> Start {
        Start {
            id: self.id.clone(),
            origin,
            args: None,
            checkout: self.checkout.clone(),
            max_launches: None,
            timeout: None,
            give_up: None,
            skip_capacity: false,
        }
    }

    /// Start the run as a process of its own; none when the fake crashed it.
    pub fn start(&self, origin: Origin) -> Option<Status> {
        self.start_with(self.input(origin))
    }

    pub fn start_with(&self, input: Start) -> Option<Status> {
        let status = caught(|| start(&self.state, &self.room, input));
        self.fake.reborn();
        status
    }

    /// An advance by a process of its own, which exits after; none when the fake crashed it.
    pub fn advance(&self) -> Option<Status> {
        let status = caught(|| advance(&self.state, &self.room, &self.id));
        self.fake.reborn();
        status
    }

    pub fn exists(&self) -> bool {
        store::load(&self.state.workflow_run(&self.id))
            .unwrap()
            .is_some()
    }

    pub fn record(&self) -> Record {
        store::load(&self.state.workflow_run(&self.id))
            .unwrap()
            .unwrap()
    }

    pub fn lines(&self) -> Vec<AnswerLine> {
        store::answers(&self.state.workflow_run(&self.id), &self.record()).unwrap()
    }

    pub fn journal(&self) -> Vec<Event> {
        store::journal(&self.state.workflow_run(&self.id), &self.record())
            .unwrap()
            .into_iter()
            .map(|entry| entry.event)
            .collect()
    }

    pub fn ending(&self) -> Option<Ending> {
        self.record().ended.map(|ended| ended.ending)
    }

    pub fn work(&self, number: u64) -> Work {
        self.record().open[&number].work.clone()
    }

    /// Change the record as a writer under the lock would.
    pub fn edit_record(&self, change: impl FnOnce(&mut Record)) {
        self.other().publish(|building| change(&mut building.next));
    }

    /// Settle command step `number` as its host would, without running it.
    pub fn settle_as_host(&self, number: u64, settled: Settled) {
        let run = Run::new(&self.state, &self.room, &self.id);
        let now = self.fake.now();
        let published = run
            .publish(|on_disk, change| {
                let open = on_disk.open.get(&number)?;
                matches!(open.work, Work::Command { .. })
                    .then(|| change.settle(number, settled, now))
            })
            .unwrap();
        assert!(
            published.is_some(),
            "step {number} is no command under a host"
        );
    }

    /// Run each host the room started, as that host, which exits after.
    pub fn run_hosts(&self) {
        self.other().run_hosts();
    }

    /// The run as another process of the room sees it, for a hook to act in.
    pub fn other(&self) -> Other {
        Other {
            state: self.state.clone(),
            room: Rc::clone(&self.room),
            fake: Rc::clone(&self.fake),
            id: self.id.clone(),
        }
    }

    /// The run's result, once it ended done.
    #[track_caller]
    pub fn done(&self) -> Value {
        match self.ending() {
            Some(Ending::Done { result }) => result,
            other => panic!("the run did not end done: {other:?}\n{:#?}", self.record()),
        }
    }

    /// The run's failure, once it ended failed.
    #[track_caller]
    pub fn failed(&self) -> (String, Option<u64>, Option<Value>) {
        match self.ending() {
            Some(Ending::Failed {
                reason,
                step,
                failure,
            }) => (reason, step, failure),
            other => panic!(
                "the run did not end failed: {other:?}\n{:#?}",
                self.record()
            ),
        }
    }
}

/// The run as another process of the room reaches it: what a hook holds, since it outlives the
/// test's borrow of its harness.
pub(super) struct Other {
    pub state: StatePaths,
    pub room: Rc<dyn Room>,
    pub fake: Rc<Fake>,
    pub id: WorkflowRunId,
}

impl Other {
    /// Run `body` as a process of its own, which exits after.
    pub fn as_process<T>(&self, body: impl FnOnce(&Self) -> anyhow::Result<T>) -> Option<T> {
        let before = self.fake.me();
        let process = self.fake.spawn();
        self.fake.me_is(&process);
        let out = caught(|| body(self));
        self.fake.kill_process(&process);
        self.fake.me_is(&before);
        out
    }

    pub fn advance(&self) -> Option<Status> {
        self.as_process(|other| advance(&other.state, &other.room, &other.id))
    }

    /// Publish `change` as a writer under the lock would.
    pub fn publish(&self, change: impl FnOnce(&mut Change)) {
        let run = Run::new(&self.state, &self.room, &self.id);
        run.publish(|_, building| {
            change(building);
            Some(())
        })
        .unwrap();
    }

    /// Run each host the room started, as that host, which exits after.
    pub fn run_hosts(&self) {
        for (pin, host) in self.fake.take_hosts() {
            let before = self.fake.me();
            self.fake.me_is(&host);
            caught(|| {
                super::command::host(&self.state, &self.room, &self.id, pin.step, pin.attempt)
            });
            self.fake.kill_process(&host);
            self.fake.me_is(&before);
        }
    }
}

/// Run `body` as a process the fake may crash: none when it did. Any other panic is the test's.
pub(super) fn caught<T>(body: impl FnOnce() -> anyhow::Result<T>) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => panic!("{error:#}"),
        Err(payload) => {
            let crashed = payload
                .downcast_ref::<String>()
                .is_some_and(|message| message.starts_with("the fake room crashed"));
            if !crashed {
                resume_unwind(payload);
            }
            None
        }
    }
}

/// Agents complete, teams read done, and hosts run.
pub(super) fn world(h: &Harness) {
    for launch in h.fake.live() {
        match launch.block {
            BlockName::Agent => h.fake.end(
                &launch.id,
                Some(LaunchEnding::Completed {
                    message: format!("{} done", launch.id),
                    answer: None,
                }),
            ),
            BlockName::Team => h
                .fake
                .board_done(launch.pin.step, launch.pin.attempt, "team done"),
            _ => {}
        }
    }
    h.run_hosts();
}

/// Advance and let `world` move until the run has ended and owes no stop.
#[track_caller]
pub(super) fn drive(h: &Harness, world: impl Fn(&Harness)) -> Ending {
    for _ in 0..60 {
        if !h.exists() {
            panic!("the run has no home");
        }
        if let Some(ended) = h.record().ended
            && h.record().owed.is_empty()
        {
            return ended.ending;
        }
        h.advance();
        world(h);
    }
    panic!("the run did not end: {:#?}", h.record());
}

pub(super) fn completed(message: &str) -> Option<LaunchEnding> {
    Some(LaunchEnding::Completed {
        message: message.to_owned(),
        answer: None,
    })
}

#[test]
fn a_run_of_a_script_that_makes_no_call_ends_done_with_its_value() {
    let h = Harness::new();
    h.start(script("return { ok: args === undefined }"));
    assert_eq!(h.done(), json!({ "ok": true }));
    assert!(h.lines().is_empty());
}

#[test]
fn a_script_that_does_not_load_refuses_the_start_and_leaves_no_home() {
    let h = Harness::new();
    let input = h.input(Origin::Script("export const meta = {\nreturn (".to_owned()));
    let refused = start(&h.state, &h.room, input).unwrap_err();
    assert!(
        format!("{refused:#}").starts_with("the workflow's script does not load: "),
        "{refused:#}"
    );
    assert!(!h.exists());
    assert_eq!(
        std::fs::read_dir(h.state.workflow_runs_dir())
            .unwrap()
            .count(),
        0
    );
    let mut input = h.input(script("return 1"));
    input.timeout = Some("soon".to_owned());
    assert!(start(&h.state, &h.room, input).is_err());
    assert_eq!(
        std::fs::read_dir(h.state.workflow_runs_dir())
            .unwrap()
            .count(),
        0,
        "a start refused for its timeout leaves no staging"
    );
}

#[test]
fn args_null_reaches_the_script_as_null() {
    let h = Harness::new();
    let mut input = h.input(script("return args === null"));
    input.args = Some(Value::Null);
    h.start_with(input);
    assert_eq!(h.done(), json!(true));
}

#[test]
fn a_quiet_advance_reads_no_line_of_the_log() {
    let h = Harness::new();
    h.start(script(
        "await now()\nawait watch({ when: 'pr=merged' })\nreturn 1",
    ));
    assert_eq!(h.record().covered, Some(1));
    let log = h.state.workflow_run(&h.id).answers(1);
    std::fs::write(&log, "").unwrap();
    assert!(advance(&h.state, &h.room, &h.id).is_ok());
}

#[test]
fn the_run_journals_the_phases_and_logs_of_the_pass_that_first_reached_them() {
    let h = Harness::new();
    h.start(script(
        "phase('one')\nlog('a')\nawait now()\nphase('two')\nlog('b')\nreturn 1",
    ));
    assert_eq!(h.done(), json!(1));
    let marks: Vec<Event> = h
        .journal()
        .into_iter()
        .filter(|event| matches!(event, Event::Phase { .. } | Event::Log { .. }))
        .collect();
    assert_eq!(
        marks,
        [
            Event::Phase {
                after: 0,
                title: "one".to_owned()
            },
            Event::Log {
                after: 0,
                text: "a".to_owned()
            },
            Event::Phase {
                after: 1,
                title: "two".to_owned()
            },
            Event::Log {
                after: 1,
                text: "b".to_owned()
            },
        ]
    );
}

#[test]
fn a_start_stages_under_a_name_of_its_own() {
    let h = Harness::new();
    let another = h
        .state
        .workflow_runs_dir()
        .join(format!(".{}.staging", h.id.as_str()));
    std::fs::create_dir_all(&another).unwrap();
    std::fs::write(another.join("record.json"), "{}").unwrap();
    h.start(script("return 1"));
    assert_eq!(h.done(), json!(1));
    assert!(
        another.join("record.json").exists(),
        "another start's staging stays"
    );
}
