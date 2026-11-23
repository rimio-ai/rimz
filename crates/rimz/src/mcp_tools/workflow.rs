//! `workflow`: the job of `rimz workflow run` up to the created run, through the one start the
//! command goes through. The answer comes once the run exists; its first pass is the engine's.

use std::path::Path;

use super::Tools;
use super::room::Room;
use super::start::{CollectedNotices, Start, TaskKind, with_warnings};
use crate::harness::workflow::{self, StartArgs};
use crate::launcher::context::resolve_caller;
use crate::launcher::workflow_start::{self, StartOrigin, WorkflowStart};
use crate::mcp::catalog::WorkflowArgs;
use crate::mcp::{Answer, Failure};
use crate::store::workflow::{RunStarter, StartCapacity};
use crate::worktree::WorktreeErr;

/// How a run's result reaches the agent that started it.
const REPORT: &str = "a WORKFLOW_REPORT message to you at your next turn boundary after the run \
                      ends";
const DETACHED_REPORT: &str = "no message when the run ends; `join` waits on it";

/// Start the deployed workflow `args` names in the caller's room, owned by the caller.
pub(super) fn start(tools: &Tools, args: &WorkflowArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.dir(None)?)?;
    if resolve_caller(&room.store)?.is_none() {
        return Err(Failure::new(
            "starting a workflow is only available to an agent RimZ can identify",
        )
        .with_hint("serve this tool to an agent RimZ launched in this room"));
    }
    let here = workflow_start::started_here(&room.store);
    if !matches!(here.starting.starter, Some(RunStarter::Agent { .. })) {
        return Err(Failure::new(
            "a workflow started here would record no agent as its starter, so nothing started",
        ));
    }
    if let Some(input) = &args.input {
        workflow::check_start_input(input)
            .map_err(|reason| Failure::new(format!("invalid input: {reason}")))?;
    }
    let start_args = StartArgs {
        input: args.input.clone(),
        deadline: parse_when("deadline", args.deadline.as_deref())?,
        give_up: parse_when("give_up", args.give_up.as_deref())?,
        capacity: if args.skip_capacity {
            StartCapacity::Skip
        } else {
            StartCapacity::Wait
        },
        detach: args.detach,
        ..StartArgs::default()
    };
    let name = Path::new(&args.name);
    let dir = workflow_start::definition_dir(name)?
        .ok_or_else(|| Failure::new(workflow_start::not_deployed(name)))?;
    let mut warnings: Vec<String> =
        workflow_start::validated(&dir, Some(&room.workspace.project_root))?
            .into_iter()
            .map(|warning| format!("warning: {warning}"))
            .collect();
    let machine = crate::config::MachineConfig::load_lenient();
    let mut notices = CollectedNotices::default();
    let started = workflow_start::start(
        WorkflowStart {
            name,
            dir,
            worktree: args.worktree.as_deref(),
            worktree_arg: "`worktree`",
            prompt: args.prompt.clone(),
            args: start_args,
            origin: StartOrigin::Here(here),
            workspace: &room.workspace,
            store: &room.store,
            machine: &machine,
        },
        &mut notices,
    );
    warnings.append(&mut notices.0);
    let failure = match started {
        Ok(Some((record, _))) => {
            let report = if args.detach { DETACHED_REPORT } else { REPORT };
            return Ok(Answer::data(&Start {
                warnings,
                ..Start::running(TaskKind::Workflow, &record.id.to_string(), report)
            }));
        }
        Ok(None) => Failure::new("the worktree was declined; nothing started"),
        Err(err)
            if matches!(
                err.downcast_ref::<WorktreeErr>(),
                Some(WorktreeErr::LaunchWorktreeRequiresRepo)
            ) =>
        {
            Failure::new("`worktree` requires a git repository-backed room")
        }
        Err(err) => Failure::from(err),
    };
    Err(with_warnings(failure, warnings))
}

pub(super) fn parse_when(
    field: &str,
    raw: Option<&str>,
) -> Result<Option<workflow::When>, Failure> {
    raw.map(|raw| {
        workflow::parse_when(raw)
            .map_err(|err| Failure::new(format!("invalid {field} `{raw}`: {err}")))
    })
    .transpose()
}
