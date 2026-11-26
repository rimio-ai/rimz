//! The seam between the server and RimZ's code: `serve` hands each parsed
//! call to the runner it was given and turns the answer into the call's result.

use std::path::PathBuf;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Map, Value};

use super::catalog::Call;

/// What carries out a tool call; `serve` calls it on a thread of the call's
/// own, so calls overlap.
pub trait Runner: Send + Sync {
    /// Carry out `call`. `cancelled` turns true once the client cancels the
    /// call or the server ends, when its reply is withheld; a call that blocks
    /// reads it at each poll and returns.
    fn run(&self, call: Call, cancelled: &dyn Fn() -> bool) -> Answer;

    /// The binary of the build the server's room moved to, when a read due by
    /// `now` finds one other than the server's own; each move is reported once.
    /// A runner that follows no room reports none.
    fn poll_build(&self, _now: Instant) -> Option<PathBuf> {
        None
    }

    /// When `poll_build` next reads the room; `None` when it follows none.
    fn next_build_poll(&self) -> Option<Instant> {
        None
    }

    /// The room's build read again once the server is quiet, after
    /// `poll_build` reported a move: its binary while it still differs from
    /// the server's own.
    fn recheck_build(&self) -> Option<PathBuf> {
        None
    }
}

/// A tool call's answer.
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// A data tool's answer, which fits the tool's output schema.
    Data(Map<String, Value>),
    /// The answer of a tool with no output schema: `lsp`, `blackboard_record`, and `guide`.
    Text(String),
    /// The call could not do its job.
    Failure(Failure),
}

impl Answer {
    /// The data answer `value` serializes to, which must be an object.
    pub fn data(value: &impl Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(Value::Object(map)) => Self::Data(map),
            Ok(other) => Self::Failure(Failure::new(format!(
                "the answer is not a JSON object: {other}"
            ))),
            Err(err) => Self::Failure(Failure::new(format!("encoding the answer: {err}"))),
        }
    }
}

/// Why a call could not do its job, the fix when there is one to name, and
/// any further fields the failure carries.
#[derive(Debug, PartialEq)]
pub struct Failure {
    pub error: String,
    pub hint: Option<String>,
    pub extra: Map<String, Value>,
}

impl Failure {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            hint: None,
            extra: Map::new(),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The failure as its result carries it: `error`, `hint` when there is
    /// one, then the extra fields.
    pub(super) fn into_value(self) -> Value {
        let mut map = Map::new();
        map.insert("error".to_owned(), Value::String(self.error));
        if let Some(hint) = self.hint {
            map.insert("hint".to_owned(), Value::String(hint));
        }
        map.extend(self.extra);
        Value::Object(map)
    }
}

impl From<anyhow::Error> for Failure {
    /// The error with every cause after it, as the command printed it.
    fn from(error: anyhow::Error) -> Self {
        Self::new(format!("{error:#}"))
    }
}
