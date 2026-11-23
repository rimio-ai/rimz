//! What a launch tool keeps of a launch's interaction: `Unattended`'s refusals, and the text of
//! each notice for the answer.

use crate::launcher::{ClosedCohort, CohortChoice, Interaction, Notice, Unattended};
use crate::mcp::Failure;

/// `Unattended`'s refusals, keeping the text of each notice for the answer.
#[derive(Default)]
pub(super) struct CollectedNotices(Vec<String>);

impl CollectedNotices {
    pub(super) fn into_lines(self) -> Vec<String> {
        self.0
    }
}

impl Interaction for CollectedNotices {
    fn cross_repo(&mut self, workspace: &crate::ResolvedWorkspace) -> anyhow::Result<bool> {
        Unattended.cross_repo(workspace)
    }
    fn pr_branch(
        &mut self,
        facts: &crate::worktree::WorktreeErr,
    ) -> anyhow::Result<Option<crate::worktree::PrBranchChoice>> {
        Unattended.pr_branch(facts)
    }
    fn unmanaged_checkout(&mut self, name: &str, cwd: &std::path::Path) -> anyhow::Result<bool> {
        Unattended.unmanaged_checkout(name, cwd)
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
