//! `shell` and `watch`: the jobs of `rimz shell` and `rimz watch`.

use std::path::Path;
use std::time::Duration;

use super::Tools;
use super::room::Room;
use super::start::{Start, TaskKind};
use crate::config::CheckOn;
use crate::harness::schedule::arm::task_label;
use crate::harness::schedule::wait::{self, WaitRequest, WaitRoom, WaitTrigger};
use crate::launcher::context::resolve_caller;
use crate::mcp::catalog::{ShellArgs, ShellOn, WatchArgs};
use crate::mcp::{Answer, Failure};
use crate::store::snapshot::SidebarSnapshot;

/// How a shell's outcome reaches the agent that started it.
const SHELL_REPORT: &str = "a message to you when the command exits; join waits for it";
/// How a watch's wake reaches the agent that armed it.
const WATCH_REPORT: &str = "a message to you when the condition reads true";

pub(super) fn shell(tools: &Tools, args: &ShellArgs) -> Result<Answer, Failure> {
    let request = WaitRequest {
        label: label(args.label.as_deref())?,
        trigger: WaitTrigger::Shell {
            command: args.command.clone(),
            on: match args.on {
                None => CheckOn::Any,
                Some(ShellOn::Fail) => CheckOn::Fail,
                Some(ShellOn::Success) => CheckOn::Success,
            },
        },
        check_in: check_in(args.check_in.as_deref())?,
    };
    arm(
        tools,
        args.cwd.as_deref(),
        request,
        TaskKind::Shell,
        SHELL_REPORT,
    )
}

pub(super) fn watch(tools: &Tools, args: &WatchArgs) -> Result<Answer, Failure> {
    let request = WaitRequest {
        label: label(args.label.as_deref())?,
        trigger: WaitTrigger::Watch {
            when: args.when.clone(),
        },
        check_in: check_in(args.check_in.as_deref())?,
    };
    arm(
        tools,
        args.cwd.as_deref(),
        request,
        TaskKind::Watch,
        WATCH_REPORT,
    )
}

fn arm(
    tools: &Tools,
    cwd: Option<&Path>,
    request: WaitRequest,
    kind: TaskKind,
    report: &str,
) -> Result<Answer, Failure> {
    request.validate()?;
    let dir = tools.dir(cwd)?;
    let room = Room::open(&dir)?;
    let caller = resolve_caller(&room.store)?;
    let snapshot = room.resolution_snapshot()?;
    let agent = wait::arming_agent(&snapshot, caller.as_ref(), request.kind())?;
    let armed = wait::arm(&wait_room(&room, &snapshot), agent, request, || {
        room.published_snapshot()
    })?;
    Ok(Answer::data(&Start::running(kind, &armed.name, report)))
}

pub(super) fn wait_room<'a>(room: &'a Room, snapshot: &'a SidebarSnapshot) -> WaitRoom<'a> {
    WaitRoom {
        workspace: &room.workspace,
        store: &room.store,
        snapshot,
        sidebar_fresh: crate::sidebar::fresh_sidebar_present,
    }
}

fn label(raw: Option<&str>) -> Result<Option<String>, Failure> {
    raw.map(|raw| {
        task_label(raw).map_err(|err| Failure::new(format!("invalid label {raw:?}: {err}")))
    })
    .transpose()
}

/// `check_in` parsed as the command parses `--check-in`.
fn check_in(raw: Option<&str>) -> Result<Option<Duration>, Failure> {
    raw.map(|raw| {
        crate::utils::time::parse_compact_duration(raw)
            .map_err(|err| Failure::new(format!("invalid check_in `{raw}`: {err}")))
    })
    .transpose()
}
