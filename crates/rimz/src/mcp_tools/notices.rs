//! What a launch tool keeps of a launch's interaction: `Unattended`'s refusals, and the text of
//! each notice for the answer.

use crate::launcher::{ClosedCohort, CohortChoice, Interaction, Notice, Unattended};
use crate::mcp::Failure;

/// `Unattended`'s refusals, keeping the text of each notice for the answer.
#[derive(Default)]
pub(super) struct CollectedNotices {
    lines: Vec<String>,
    closed_cohort: Option<String>,
}

impl CollectedNotices {
    /// The worktree whose closed cohort this launch refused to enter.
    pub(super) fn closed_cohort(&self) -> Option<&str> {
        self.closed_cohort.as_deref()
    }

    pub(super) fn into_lines(self) -> Vec<String> {
        self.lines
    }
}

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
        self.closed_cohort = Some(cohort.name.to_owned());
        Unattended.closed_cohort(cohort)
    }
    fn notice(&mut self, notice: Notice<'_>) -> anyhow::Result<()> {
        self.lines.extend(notice.line());
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
