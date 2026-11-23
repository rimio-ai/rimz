//! The answer of every tool that starts something, and the one place an id is built and
//! parsed.

use serde::Serialize;

use crate::launcher::{ClosedCohort, CohortChoice, Interaction, Notice, Unattended};
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
    Wait,
}

impl TaskKind {
    const ALL: [Self; 6] = [
        Self::Subagent,
        Self::Agent,
        Self::Team,
        Self::Workflow,
        Self::Schedule,
        Self::Wait,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Workflow => "workflow",
            Self::Schedule => "schedule",
            Self::Wait => "wait",
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
                    "an id is a kind (subagent, agent, team, workflow, schedule, or wait), a \
                     colon, and a name, as `tasks` lists it; end a task or subagent your \
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
            StartedKind::Wait => Self::Wait,
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
    pub report: &'static str,
    /// What the launch changed or skipped of what was asked, each as the command prints it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Start {
    pub fn running(kind: TaskKind, name: &str, report: &'static str) -> Self {
        let id = kind.id(name);
        Self {
            stop: TaskKind::stop_call(&id),
            id,
            status: "running",
            report,
            warnings: Vec::new(),
        }
    }
}

/// `Unattended`'s refusals, keeping the text of each notice for the answer.
#[derive(Default)]
pub(super) struct CollectedNotices(pub(super) Vec<String>);

impl Interaction for CollectedNotices {
    fn cross_repo(&mut self, workspace: &crate::ResolvedWorkspace) -> anyhow::Result<bool> {
        Unattended.cross_repo(workspace).map_err(|_| {
            anyhow::anyhow!(
                "this agent's checkout is in another repository than room root {}; `worktree` \
                 works only from the room's checkout",
                workspace.project_root.display()
            )
        })
    }
    fn pr_branch(
        &mut self,
        facts: &crate::worktree::WorktreeErr,
    ) -> anyhow::Result<Option<crate::worktree::PrBranchChoice>> {
        Unattended.pr_branch(facts)
    }
    fn unmanaged_checkout(&mut self, name: &str, _cwd: &std::path::Path) -> anyhow::Result<bool> {
        anyhow::bail!(
            "worktree `{name}` is not RimZ-managed, and a tool call cannot confirm entering it"
        )
    }
    fn closed_cohort(&mut self, cohort: ClosedCohort<'_>) -> anyhow::Result<CohortChoice> {
        Unattended.closed_cohort(cohort)
    }
    fn notice(&mut self, notice: Notice<'_>) -> anyhow::Result<()> {
        self.0.extend(notice.line());
        Ok(())
    }
}

/// `failure` with the notices collected before it, which the command would already have
/// printed; a failure with none keeps its plain shape.
pub(super) fn with_warnings(mut failure: Failure, warnings: Vec<String>) -> Failure {
    if !warnings.is_empty() {
        failure
            .extra
            .insert("warnings".to_owned(), serde_json::json!(warnings));
    }
    failure
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The confirmations a tool call cannot give are refused in its own terms: no flag it
    /// does not have, and no terminal it is not in.
    #[test]
    fn an_unattended_refusal_names_nothing_a_tool_caller_lacks() {
        let project_root = PathBuf::from("/rooms/marker");
        let workspace = crate::ResolvedWorkspace {
            workspace_id: crate::WorkspaceId::from_project_root(&project_root),
            project_root,
            cwd_project_root: Some(PathBuf::from("/repos/current")),
            root_class: crate::workspace::RootClass::Marker,
            worktree_root: PathBuf::from("/repos/current"),
            worktree_branch: Some("main".to_owned()),
            session_name: "rimz-room".to_owned(),
            mux_hint: None,
        };
        let mut notices = CollectedNotices::default();
        assert_eq!(
            notices.cross_repo(&workspace).unwrap_err().to_string(),
            "this agent's checkout is in another repository than room root /rooms/marker; \
             `worktree` works only from the room's checkout"
        );
        assert_eq!(
            notices
                .unmanaged_checkout("feat", std::path::Path::new("/repos/feat"))
                .unwrap_err()
                .to_string(),
            "worktree `feat` is not RimZ-managed, and a tool call cannot confirm entering it"
        );
    }
}
