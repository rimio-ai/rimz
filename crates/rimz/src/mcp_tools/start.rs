//! The answer of every tool that starts something, and the one place an id is built and
//! parsed.

use serde::Serialize;

use crate::mcp::Failure;
use crate::mcp::catalog::StartedKind;

/// A thing a tool starts, named in an id by its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Subagent,
    Agent,
    Team,
    Workflow,
    Schedule,
    Shell,
    Watch,
}

impl TaskKind {
    const ALL: [Self; 7] = [
        Self::Subagent,
        Self::Agent,
        Self::Team,
        Self::Workflow,
        Self::Schedule,
        Self::Shell,
        Self::Watch,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Workflow => "workflow",
            Self::Schedule => "schedule",
            Self::Shell => "shell",
            Self::Watch => "watch",
        }
    }

    /// The id of the thing of this kind named `name`: the kind, a colon, the name.
    pub fn id(self, name: &str) -> String {
        format!("{}:{name}", self.as_str())
    }

    /// The kind and name an id holds. An id of another shape, such as a provider's own task
    /// id, is refused with the tool that ends it.
    pub fn parse(id: &str) -> Result<(Self, &str), Failure> {
        id.split_once(':')
            .and_then(|(kind, name)| {
                let kind = Self::ALL.into_iter().find(|known| known.as_str() == kind)?;
                (!name.is_empty()).then_some((kind, name))
            })
            .ok_or_else(|| {
                Failure::new(format!("`{id}` is no id of a thing RimZ started")).with_hint(
                    "an id is a kind (subagent, agent, team, workflow, schedule, shell, or \
                     watch), a colon, and a name, as `tasks` lists it; end a task or subagent your \
                     provider started with your provider's own tool",
                )
            })
    }

    /// The call that stops the thing `id` names.
    pub fn stop_call(id: &str) -> String {
        format!("stop({{id: \"{id}\"}})")
    }
}

impl From<StartedKind> for TaskKind {
    fn from(kind: StartedKind) -> Self {
        match kind {
            StartedKind::Subagent => Self::Subagent,
            StartedKind::Agent => Self::Agent,
            StartedKind::Team => Self::Team,
            StartedKind::Workflow => Self::Workflow,
            StartedKind::Schedule => Self::Schedule,
            StartedKind::Shell => Self::Shell,
            StartedKind::Watch => Self::Watch,
        }
    }
}

/// What a start answers: the thing's id, that it runs, the exact call that
/// stops it, and how its result reaches an agent that does not block.
#[derive(Debug, Serialize)]
pub struct Start {
    pub id: String,
    pub status: &'static str,
    pub stop: String,
    pub report: String,
    /// What the launch changed or skipped of what was asked, each as the command prints it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Start {
    pub fn running(kind: TaskKind, name: &str, report: impl Into<String>) -> Self {
        let id = kind.id(name);
        Self {
            stop: TaskKind::stop_call(&id),
            id,
            status: "running",
            report: report.into(),
            warnings: Vec::new(),
        }
    }
}
