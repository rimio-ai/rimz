//! The seam between a run and the world. The room carries facts and acts; every decision is the
//! engine's. No call that launches, stops, books, or starts a host is made while the engine holds
//! the workspace lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use crate::harness::block::BlockName;
use crate::ids::WorkflowRunId;
use crate::store::workflow_run::Process;

/// What every launch of a run's step carries, so the run finds it again by it alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    pub run: WorkflowRunId,
    pub step: u64,
    pub attempt: u32,
}

/// Whether the room would launch a step now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Admit {
    Go,
    /// Not now, for this reason: the profile's headroom, the machine's caps.
    Hold(String),
    /// Never, in this sentence.
    Refuse(String),
}

/// What a launch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Launched {
    /// The launch ids it opened, the leader first, and an agent's handle.
    Started {
        launches: Vec<String>,
        handle: Option<String>,
    },
    /// Nothing was launched, for this reason.
    Refused(String),
    /// Whether anything was launched is not known.
    Unknown(String),
}

/// How a launch ended, when it left an ending.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ending {
    /// Its last message, and the answer it handed in.
    Completed {
        message: String,
        answer: Option<Value>,
    },
    Failed(String),
    Canceled(String),
}

/// A launch under a pin of the run, a launch a rebirth restored included.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Seen {
    pub pin: Pin,
    pub launch: String,
    pub handle: Option<String>,
    pub alive: bool,
    pub ending: Option<Ending>,
}

/// One look at a condition: its truth, none when a key it needs is unread, and the reading of
/// every key it names, `None` for a key the look did not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    pub truth: Option<bool>,
    pub readings: BTreeMap<String, Option<String>>,
}

pub(crate) trait Room {
    fn now(&self) -> Timestamp;
    /// The time zone a calendar `give_up` resolves in.
    fn zone(&self) -> jiff::tz::TimeZone;
    /// This process.
    fn me(&self) -> Process;
    fn alive(&self, process: &Process) -> bool;
    /// Whether the room would launch the agent or team step that `object` calls, as the script
    /// passed it, into `checkout` now: the call's profile or team decides its headroom, the
    /// machine's caps hold every launch, and a call the room can never launch is refused.
    fn admit(&self, block: BlockName, object: &Value, checkout: &Path) -> Admit;
    /// Launch an agent or a team as the script passed it, into the run's checkout, under `pin`,
    /// with no calling agent: the run is its owner, and a checkout whose earlier agents have closed
    /// gets a fresh start. An agent's `schema` reaches the launch's call record; its `timeout`
    /// does not, since the run enforces it.
    fn launch(&self, block: BlockName, object: &Value, checkout: &Path, pin: &Pin) -> Launched;
    /// Every launch that carries a pin of `run`.
    fn launches(&self, run: &WorkflowRunId) -> anyhow::Result<Vec<Seen>>;
    /// The leader's last message once the board of the team under `pin` has read done.
    fn board(&self, pin: &Pin) -> Option<String>;
    fn stop_launches(&self, launches: &[String]) -> anyhow::Result<()>;
    /// Stop `process`, or the group it leads, only while its identity still matches.
    fn kill(&self, process: &Process, group: bool) -> anyhow::Result<()>;
    /// Start a detached command host for the step under `pin`.
    fn start_host(&self, pin: &Pin) -> anyhow::Result<Process>;
    /// One look at the condition `when` from `dir`.
    fn look(&self, when: &str, dir: &Path) -> Look;
    /// Book a `schedule` call for the run: the booking's id, or the sentence that refuses it.
    fn book(&self, run: &WorkflowRunId, object: &Value) -> Result<String, String>;
    /// The directory of the installed workflow `name`.
    fn installed(&self, name: &str) -> Option<PathBuf>;
}
