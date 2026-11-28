//! The engine of a workflow script: a sandboxed JavaScript context, the loader, and the replay.
//!
//! A replay runs the script from the top in a fresh context, makes every call a block of
//! `harness::block` named a pending promise with the next number, and hands the log's answers
//! back one at a time in the order they arrived. Everything a script learns about the world comes
//! from those answers, so a second replay of the same inputs makes the same calls with the same
//! numbers: the context has no clock but the `now` block, no time zone, no randomness, no timers,
//! no weak references, no module loader, and no host access but the blocks, `file`, and `args`.
//! Its `ms` and `reads` are pure, so they make no call, its `phase` and `log` only mark the pass
//! for display, and its `cases` calls only `watch` and `now`. A `workflow` call runs the body of
//! the script it names in the same context, at once, its calls numbered in the run beside the
//! caller's and its return or throw settling the call.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::block::{BlockName, Cause, Failure, WorkflowSource};

mod engine;
mod heap;
mod load;

pub use load::Meta;

/// The bounds of one pass of a script.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Wall time, through the engine's interrupt handler.
    pub time: Duration,
    /// The engine's own heap, in bytes (not the process's RSS), counted by its allocator, with the
    /// marks the pass keeps.
    pub memory: usize,
}

impl Caps {
    /// The caps of every pass a command runs.
    pub const PASS: Self = Self {
        time: Duration::from_secs(30),
        memory: 256 << 20,
    };
}

/// How a call settled: `{"result": ...}` or `{"failure": {...}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Outcome {
    Result(Value),
    Failure(Failure),
}

/// One line of a run's answer log: the call it settled, and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub number: u64,
    pub block: BlockName,
    pub digest: String,
    pub outcome: Outcome,
}

/// The answers a replay hands back, in the order they arrived.
#[derive(Clone, Debug)]
pub enum Log {
    /// A run's log: each line names its call, and a call that differs from it is `replay`.
    Recorded(Vec<Line>),
    /// Answers written by hand: line n answers call n, whatever the call passed.
    Answers(Vec<Outcome>),
}

/// What one pass reads.
pub struct Pass<'a> {
    /// The script file's name, which its line numbers carry.
    pub name: &'a str,
    pub text: &'a str,
    /// The global `args`; `undefined` without one.
    pub args: Option<&'a Value>,
    pub log: &'a Log,
    pub caps: Caps,
    /// The workflow's directory, which `file` reads; a workflow without one refuses `file`.
    pub dir: Option<&'a Path>,
    /// What reads the script a `workflow` call names; without one such a call is refused.
    pub reader: Option<Rc<Reader>>,
    /// How many of the log's lines the run's last pass covered; none before its first pass.
    pub covered: Option<usize>,
    /// The run's step cap: the call that would take the number past it is not made.
    pub steps: Option<u64>,
}

/// A `workflow` call, as the reader of the script it names is handed it.
pub struct Calling<'a> {
    pub number: u64,
    /// The `workflow` call whose body made this one; none for a call of the run's own script.
    pub parent: Option<u64>,
    pub source: WorkflowSource,
    pub value: &'a str,
}

/// The script a `workflow` call names, as its reader found it.
pub struct Called {
    /// The file name its line numbers carry.
    pub name: String,
    pub text: String,
    /// The directory its `file` reads; none for a workflow that is one file or text.
    pub dir: Option<PathBuf>,
}

/// Reads the script of a `workflow` call, or says in a sentence why it cannot.
pub type Reader = dyn Fn(&Calling<'_>) -> Result<Called, String>;

/// One call of a block, in the order the script made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub number: u64,
    pub block: BlockName,
    pub object: Value,
    pub digest: String,
    /// The `workflow` call whose body made this call; none for a call of the run's own script.
    pub parent: Option<u64>,
    /// The title of the script's last `phase` when it made the call. The digest leaves it out,
    /// so a renamed phase keeps a log's answers.
    pub phase: Option<Arc<str>>,
}

/// A `phase` or `log` the script made. Both are display alone: neither is a call, takes a
/// number, or gets an answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    /// How many calls the script had made before it.
    pub after: u64,
    /// Made with no more of the log's lines handed back than the run's last pass covered, so
    /// that pass made it too: a reader of every pass of a run keeps the marks not replayed, and
    /// none repeats.
    pub replayed: bool,
    pub progress: Progress,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    /// The run's stage from here until the next `phase`, marked only when the title changes.
    Phase(Arc<str>),
    /// One narrator line.
    Log(String),
}

/// What a replay found.
#[derive(Debug)]
pub struct Replay {
    pub meta: Meta,
    pub calls: Vec<Call>,
    /// The calls the log does not answer, by number, a `workflow` call among them until its body
    /// ends.
    pub open: Vec<u64>,
    /// How many of the log's lines were handed back before the script could run no further.
    pub used: usize,
    /// Every `phase` change and `log` line, in the order the script made them.
    pub marks: Vec<Mark>,
    pub end: End,
}

/// Where a replay leaves the run.
#[derive(Debug, PartialEq)]
pub enum End {
    /// The script waits on a call the log does not answer.
    Pending,
    /// The body returned this value (`null` for `undefined`).
    Returned(Value),
    /// A call's failure nothing caught, or a failure of the pass itself (`replay`, `stuck`,
    /// `steps`, or `invalid` for a cap).
    Failed(Failure),
    /// The script's own uncaught throw: its message and its line.
    Threw(String),
}

/// Run `pass` once against its log. An error is a script that cannot load, with its line.
pub fn replay(pass: &Pass<'_>) -> Result<Replay, String> {
    let loaded = load::load(pass.text)?;
    engine::run(pass, loaded)
}

/// The lowest-numbered call a resume of a run must run again (design 7.2), from a replay of the
/// run's inputs over `log` and the calls that were still open when it ended: the first call that
/// differs from the log, a call with no answer, or the call whose failure ended the run. A call
/// the replay left open whose answer is anywhere in a recorded log keeps it: the replay stopped
/// handing back lines before that one arrived. A `workflow` call is never one to run for being
/// open: its body's calls are. None when every call keeps its answer.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "a resume of a run reads it once runs are built")
)]
pub(in crate::harness) fn first_to_run(
    replay: &Replay,
    log: &Log,
    open_at_end: &[u64],
) -> Option<u64> {
    let ended_by = match &replay.end {
        End::Failed(failure) => failure.step,
        End::Pending | End::Returned(_) | End::Threw(_) => None,
    };
    let logged = |number: &u64| match log {
        Log::Recorded(lines) => lines.iter().any(|line| line.number == *number),
        Log::Answers(_) => false,
    };
    let step = |number: &&u64| {
        !replay
            .calls
            .iter()
            .any(|call| call.number == **number && call.block == BlockName::Workflow)
    };
    replay
        .open
        .iter()
        .filter(|number| !logged(number))
        .chain(open_at_end)
        .filter(step)
        .copied()
        .chain(ended_by)
        .min()
}

/// The answers of a hand-written answers file: JSON Lines, line n answering call n, blank lines
/// skipped. A failure may leave out `reason`, and the engine fills its `block` and `step`; its
/// cause is one a call can carry, not `replay`, `stuck`, `launches`, or `steps`.
pub fn answers(text: &str) -> Result<Vec<Outcome>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let at = |error: serde_json::Error| format!("line {}: {error}", index + 1);
            let mut value: Value = serde_json::from_str(line).map_err(at)?;
            if let Some(Value::Object(failure)) = value.get_mut("failure")
                && !failure.contains_key("reason")
            {
                let cause = failure.get("cause").and_then(Value::as_str).unwrap_or("?");
                let reason = format!("the answers file fails this call with `{cause}`");
                failure.insert("reason".to_owned(), Value::String(reason));
            }
            let outcome = serde_json::from_value(value).map_err(at)?;
            if let Outcome::Failure(failure) = &outcome
                && matches!(
                    failure.cause,
                    Cause::Replay | Cause::Stuck | Cause::Launches | Cause::Steps
                )
            {
                return Err(format!(
                    "line {}: a call cannot fail with `{}`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`",
                    index + 1,
                    failure.cause.as_str()
                ));
            }
            Ok(outcome)
        })
        .collect()
}

fn pass_failure(cause: Cause, reason: String, step: Option<u64>) -> Failure {
    Failure {
        block: None,
        cause,
        reason,
        step,
        code: None,
        stdout: None,
        output: None,
        handle: None,
    }
}

#[cfg(test)]
mod tests;
