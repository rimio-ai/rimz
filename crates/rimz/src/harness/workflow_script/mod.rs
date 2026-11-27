//! The engine of a workflow script: a sandboxed JavaScript context, the loader, and the replay.
//!
//! A replay runs the script from the top in a fresh context, makes every call a block of
//! `harness::block` named a pending promise with the next number, and hands the log's answers
//! back one at a time in the order they arrived. Everything a script learns about the world comes
//! from those answers, so a second replay of the same inputs makes the same calls with the same
//! numbers: the context has no clock but the `now` block, no time zone, no randomness, no timers,
//! no weak references, no module loader, and no host access but the blocks, `file`, and `args`.
//! Its `ms` and `reads` are pure, so they make no call.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::block::{BlockName, Cause, Failure};

mod engine;
mod heap;
mod load;

pub use load::Meta;

/// The bounds of one pass of a script.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Wall time, through the engine's interrupt handler.
    pub time: Duration,
    /// The engine's own heap, in bytes (not the process's RSS), counted by its allocator.
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
}

/// One call of a block, in the order the script made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub number: u64,
    pub block: BlockName,
    pub object: Value,
    pub digest: String,
}

/// What a replay found.
#[derive(Debug)]
pub struct Replay {
    pub meta: Meta,
    pub calls: Vec<Call>,
    /// The calls the log does not answer, by number.
    pub open: Vec<u64>,
    /// How many of the log's lines were handed back before the script could run no further.
    pub used: usize,
    pub end: End,
}

/// Where a replay leaves the run.
#[derive(Debug, PartialEq)]
pub enum End {
    /// The script waits on a call the log does not answer.
    Pending,
    /// The body returned this value (`null` for `undefined`).
    Returned(Value),
    /// A call's failure nothing caught, or a failure of the pass itself (`replay`, `stuck`, or
    /// `invalid` for a cap).
    Failed(Failure),
    /// The script's own uncaught throw: its message and its line.
    Threw(String),
}

/// Run `pass` once against its log. An error is a script that cannot load, with its line.
pub fn replay(pass: &Pass<'_>) -> Result<Replay, String> {
    let loaded = load::load(pass.text)?;
    engine::run(pass, loaded)
}

/// The lowest-numbered call a resume of a run must run again (design 7.2), from the run's inputs
/// and the calls that were still open when it ended: the first call that differs from the log,
/// a call with no answer, or the call whose failure ended the run. A call the replay left open
/// whose answer is anywhere in a recorded log keeps it: the replay stopped handing back lines
/// before that one arrived. None when every call keeps its answer.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "a resume of a run reads it once runs are built")
)]
pub(in crate::harness) fn first_to_run(
    pass: &Pass<'_>,
    open_at_end: &[u64],
) -> Result<Option<u64>, String> {
    let replay = replay(pass)?;
    let ended_by = match &replay.end {
        End::Failed(failure) => failure.step,
        End::Pending | End::Returned(_) | End::Threw(_) => None,
    };
    let logged = |number: &u64| match pass.log {
        Log::Recorded(lines) => lines.iter().any(|line| line.number == *number),
        Log::Answers(_) => false,
    };
    Ok(replay
        .open
        .iter()
        .filter(|number| !logged(number))
        .chain(open_at_end)
        .copied()
        .chain(ended_by)
        .min())
}

/// The answers of a hand-written answers file: JSON Lines, line n answering call n, blank lines
/// skipped. A failure may leave out `reason`, and the engine fills its `block` and `step`; its
/// cause is one a call can carry, not `replay` or `stuck`.
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
                && matches!(failure.cause, Cause::Replay | Cause::Stuck)
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
