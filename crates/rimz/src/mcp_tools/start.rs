//! The answer of every tool that starts something.

use serde::Serialize;

/// A thing a tool starts, named in an id by its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Wait,
}

impl TaskKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
        }
    }

    /// The id of the thing of this kind named `name`: the kind, a colon, the name.
    pub fn id(self, name: &str) -> String {
        format!("{}:{name}", self.as_str())
    }
}

/// What a start answers: the thing's id, that it runs, the exact call that
/// stops it, and how its result reaches an agent that does not block.
#[derive(Debug, Serialize)]
pub struct Start {
    pub id: String,
    pub status: &'static str,
    pub stop: String,
    pub report: &'static str,
}

impl Start {
    pub fn running(kind: TaskKind, name: &str, stop: String, report: &'static str) -> Self {
        Self {
            id: kind.id(name),
            status: "running",
            stop,
            report,
        }
    }
}
