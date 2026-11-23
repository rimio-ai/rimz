//! `subagent`: the job of `rimz subagents <profile> <prompt>`, launched through the one
//! background launch the command goes through.

use super::Tools;
use super::room::Room;
use super::start::{Start, TaskKind};
use crate::harness::run::SupervisedRunRequest;
use crate::launcher::context::resolve_caller;
use crate::launcher::{
    BackgroundLaunchOutcome, ClosedCohort, CohortChoice, Interaction, Notice, RunCompletion,
    SupervisedContext, Unattended,
};
use crate::mcp::catalog::SubagentArgs;
use crate::mcp::{Answer, Failure};

/// How a subagent's result reaches the agent that launched it.
const REPORT: &str =
    "a SUBAGENT_REPORT message to you once every subagent you have running settles";

/// `Unattended`'s refusals, keeping the text of each notice for the answer.
#[derive(Default)]
struct CollectedNotices(Vec<String>);

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

/// Launch the subagent in the caller's room. A call without `cwd` leaves the child's
/// directory to the launch, as the command does: the caller's own checkout, or the profile's
/// empty directory.
pub(super) fn launch(tools: &Tools, args: &SubagentArgs) -> Result<Answer, Failure> {
    let cwd = args
        .cwd
        .as_deref()
        .map(|cwd| tools.dir(Some(cwd)))
        .transpose()?;
    let room = Room::open(&tools.dir(None)?)?;
    if resolve_caller(&room.store)?.is_none() {
        return Err(Failure::new(
            "launching a subagent is only available to an agent RimZ can identify",
        )
        .with_hint("serve this tool to an agent RimZ launched in this room"));
    }
    let mut request = SupervisedRunRequest::subagent(args.profile.clone(), args.prompt.clone());
    request.cwd = cwd;
    request.description.clone_from(&args.description);
    request.model.clone_from(&args.model);
    request.effort.clone_from(&args.effort);
    request.agent = crate::harness::plan::normalized_preset_value(args.agent.as_deref());
    request.timeout = args
        .timeout
        .as_deref()
        .map(|raw| {
            crate::utils::time::parse_compact_duration(raw)
                .map_err(|err| Failure::new(format!("invalid timeout `{raw}`: {err}")))
        })
        .transpose()?;
    let context = SupervisedContext {
        root: Some(room.workspace.project_root.clone()),
        worktree_fresh_command: None,
        machine: crate::config::MachineConfig::load_lenient(),
        mux: None,
        recovery: crate::room::AttendedRecovery::RequireExplicitReset,
        completion: RunCompletion::Detached,
    };
    let mut notices = CollectedNotices::default();
    let outcome = crate::launcher::launch_background(&request, context, &mut notices);
    let warnings = notices.0;
    let failure = match outcome {
        Ok(BackgroundLaunchOutcome::Launched(child)) => {
            return Ok(Answer::data(&Start {
                warnings,
                ..Start::running(TaskKind::Subagent, &child.name, REPORT)
            }));
        }
        Ok(BackgroundLaunchOutcome::BudgetExceeded { reason }) => Failure::new(reason),
        Ok(BackgroundLaunchOutcome::Aborted) => Failure::new(
            "the launch was declined or found an open launch of the same name; nothing launched",
        )
        .with_hint("call `tasks` to see what you have running"),
        Err(err) => Failure::from(err),
    };
    Err(with_warnings(failure, warnings))
}

/// `failure` with the notices collected before it, which the command would already have
/// printed; a failure with none keeps its plain shape.
fn with_warnings(mut failure: Failure, warnings: Vec<String>) -> Failure {
    if !warnings.is_empty() {
        failure
            .extra
            .insert("warnings".to_owned(), serde_json::json!(warnings));
    }
    failure
}
