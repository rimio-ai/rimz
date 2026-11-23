//! `subagent`: the job of `rimz subagents <profile> <prompt>`, launched through the one
//! background launch the command goes through.

use super::Tools;
use super::notices::{CollectedNotices, with_warnings};
use super::room::Room;
use super::start::{Start, TaskKind};
use crate::harness::run::SupervisedRunRequest;
use crate::launcher::context::resolve_caller;
use crate::launcher::{BackgroundLaunchOutcome, RunCompletion, SupervisedContext};
use crate::mcp::catalog::SubagentArgs;
use crate::mcp::{Answer, Failure};

/// How a subagent's result reaches the agent that launched it.
const REPORT: &str =
    "a SUBAGENT_REPORT message to you once every subagent you have running settles";

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
    let warnings = notices.into_lines();
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
