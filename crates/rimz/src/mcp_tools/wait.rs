//! `wait`, `wait_list`, and `wait_cancel`: the jobs of `rimz wait`, `rimz wait
//! list`, and `rimz wait cancel`.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use serde::Serialize;

use super::Tools;
use super::room::Room;
use super::start::{Start, TaskKind};
use crate::config::CheckOn;
use crate::harness::schedule::arm::{TaskName, task_label};
use crate::harness::schedule::wait::{self, CancelTarget, WaitRequest, WaitRoom, WakeRow};
use crate::launcher::context::resolve_caller;
use crate::mcp::catalog::{WaitArgs, WaitCancelArgs, WaitOn, WaitTrigger};
use crate::mcp::{Answer, Failure};
use crate::store::snapshot::SidebarSnapshot;

/// How a wait's result reaches the agent that armed it.
const REPORT: &str = "a message to you when the wait fires";

pub(super) fn arm(tools: &Tools, args: &WaitArgs) -> Result<Answer, Failure> {
    let dir = tools.dir(args.cwd.as_deref())?;
    let request = request(args)?;
    let room = Room::open(&dir)?;
    let caller = resolve_caller(&room.store)?;
    let snapshot = room.resolution_snapshot()?;
    let agent = wait::arming_agent(&snapshot, caller.as_ref())?;
    let armed = wait::arm(&wait_room(&room, &snapshot), agent, request, &dir, || {
        room.published_snapshot()
    })?;
    let stop = format!("wait_cancel({{name: \"{}\"}})", armed.name);
    Ok(Answer::data(&Start::running(
        TaskKind::Wait,
        &armed.name,
        stop,
        REPORT,
    )))
}

#[derive(Serialize)]
struct Waits {
    waits: Vec<WakeRow>,
}

pub(super) fn list(tools: &Tools) -> Result<Answer, Failure> {
    let room = Room::open(&tools.start_dir)?;
    let caller = resolve_caller(&room.store)?;
    let snapshot = room.resolution_snapshot()?;
    let agent = wait::caller_agent(&snapshot, caller.as_ref(), None)?;
    let waits = wait::list(&wait_room(&room, &snapshot), agent)?;
    Ok(Answer::data(&Waits { waits }))
}

pub(super) fn cancel(tools: &Tools, args: &WaitCancelArgs) -> Result<Answer, Failure> {
    let target = match &args.name {
        Some(name) => CancelTarget::Name(
            TaskName::from_str(name)
                .map_err(|err| Failure::new(format!("invalid wait name `{name}`: {err}")))?,
        ),
        None => CancelTarget::AllWaits,
    };
    let room = Room::open(&tools.start_dir)?;
    let caller = resolve_caller(&room.store)?;
    let snapshot = room.resolution_snapshot()?;
    let agent = wait::canceling_agent(&snapshot, caller.as_ref())?;
    let canceled = wait::cancel(&wait_room(&room, &snapshot), agent, target)?;
    Ok(Answer::data(&canceled))
}

fn wait_room<'a>(room: &'a Room, snapshot: &'a SidebarSnapshot) -> WaitRoom<'a> {
    WaitRoom {
        workspace: &room.workspace,
        store: &room.store,
        snapshot,
        sidebar_fresh: crate::sidebar::fresh_sidebar_present,
    }
}

/// The wait the arguments arm: the trigger's value under its own field, and
/// each duration parsed as the command parses it.
fn request(args: &WaitArgs) -> Result<WaitRequest, Failure> {
    let duration = |field: &str, raw: &str| -> Result<Duration, Failure> {
        crate::utils::time::parse_compact_duration(raw)
            .map_err(|err| Failure::new(format!("invalid {field} `{raw}`: {err}")))
    };
    let label = args
        .label
        .as_deref()
        .map(|raw| {
            task_label(raw).map_err(|err| Failure::new(format!("invalid label {raw:?}: {err}")))
        })
        .transpose()?;
    let mut request = WaitRequest {
        label,
        grep: args.grep.clone(),
        every: args
            .every
            .as_deref()
            .map(|raw| duration("every", raw))
            .transpose()?,
        on: args.on.map(|on| match on {
            WaitOn::Fail => CheckOn::Fail,
            WaitOn::Success => CheckOn::Success,
        }),
        check_in: args
            .check_in
            .as_deref()
            .map(|raw| duration("check_in", raw))
            .transpose()?,
        ..WaitRequest::default()
    };
    let value = args.value.clone();
    match args.trigger {
        WaitTrigger::Run => request.run = Some(value),
        WaitTrigger::Check => request.check = Some(value),
        WaitTrigger::File => request.file = Some(PathBuf::from(value)),
        WaitTrigger::Pid => {
            let pid = value
                .parse::<u32>()
                .ok()
                .filter(|pid| (1..=i32::MAX.unsigned_abs()).contains(pid))
                .ok_or_else(|| {
                    Failure::new(format!(
                        "invalid pid `{value}`: a process id from 1 to {}",
                        i32::MAX
                    ))
                })?;
            request.pid = Some(pid);
        }
        WaitTrigger::When => request.when = vec![value],
        WaitTrigger::In => request.in_after = Some(duration("in", &value)?),
        WaitTrigger::At => request.at = Some(value),
    }
    Ok(request)
}
