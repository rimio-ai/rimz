//! `rimz workflow wait`: block until named workflow runs end, or one needs a person.

use std::time::Duration;

use rimz::harness::workflow::join::{self, Ended, JoinTarget, Joined, Reading};
use rimz::store::workflow::{RunClass, RunNames, RunState};

use super::*;
use crate::cli::render::announce::{MESSAGE_STOP_EXIT, write_message_stop};
use crate::cli::usage::UsageError;
use rimz::message::announce::WaitCaller;

/// The exit of a wait that returned because an awaited run needs a person.
const NEEDS_YOU_EXIT: i32 = 3;
const TIMED_OUT_EXIT: i32 = 124;

struct Target {
    address: String,
    shown: String,
    join: JoinTarget,
}

fn ended_exit(ended: &Ended) -> i32 {
    i32::from(ended.state != RunState::Done)
}

pub(super) fn run(
    references: &[String],
    any: bool,
    timeout: Option<Duration>,
    json: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let mut caller = WaitCaller::starting_now();
    let ctx = super::super::Ctx::open(globals).ok();
    let mut targets = references
        .iter()
        .map(|raw| target(raw, globals))
        .collect::<Result<Vec<_>>>()?;
    let mut joins = targets
        .iter()
        .map(|target| target.join.clone())
        .collect::<Vec<_>>();
    let joined = join::join(&mut joins, any, timeout, &mut || {
        let ctx = ctx.as_ref()?;
        caller.queued(&ctx.store, &ctx.workspace.session_name)
    })?;
    for (target, join) in targets.iter_mut().zip(joins) {
        target.join = join;
    }
    match joined {
        Joined::Ended { first: Some(index) } => {
            let target = &targets[index];
            report(std::slice::from_ref(target), json, 0)?;
            std::process::exit(exit_of(target, 0));
        }
        Joined::Ended { first: None } => {
            report(&targets, json, 0)?;
            std::process::exit(
                targets
                    .iter()
                    .map(|target| exit_of(target, 0))
                    .max()
                    .unwrap_or(0),
            );
        }
        Joined::NeedsYou => {
            report(&targets, json, NEEDS_YOU_EXIT)?;
            std::process::exit(NEEDS_YOU_EXIT);
        }
        Joined::Interrupted(announcement) => {
            report(&targets, json, MESSAGE_STOP_EXIT)?;
            write_message_stop(&mut render::err(), &announcement)?;
            std::process::exit(MESSAGE_STOP_EXIT);
        }
        Joined::StillRunning => {
            report(&targets, json, TIMED_OUT_EXIT)?;
            let timeout = timeout.unwrap_or_default();
            writeln!(
                render::err(),
                "timed out after {}; the runs still going are left running",
                render::age_label(timeout.as_secs())
            )?;
            std::process::exit(TIMED_OUT_EXIT);
        }
        Joined::Unreadable(unreadable) => Err(refusal(*unreadable)),
    }
}

/// A target's exit under a wait that stops with `stop`: a run still going carries the stop's.
fn exit_of(target: &Target, stop: i32) -> i32 {
    match &target.join.reading {
        Reading::Ended(ended) => ended_exit(ended),
        Reading::NeedsYou { .. } => NEEDS_YOU_EXIT,
        Reading::Running { .. } => stop,
    }
}

/// The run a reference names, by id or address. An id found in no home is a usage error, and a
/// bare workflow name names no one run.
fn target(raw: &str, globals: &super::super::GlobalFlags) -> Result<Target> {
    if let Ok(id) = raw.parse::<WorkflowRunId>() {
        let Some((state, found)) = records::find_under(&paths::rimz_home(), &id)? else {
            return Err(UsageError::new(format!("unknown workflow run {id}")).into());
        };
        let reading = look(&id)?;
        let address = match found.class {
            RunClass::Current(record) => record.address().to_string(),
            _ => id.to_string(),
        };
        return Ok(Target {
            address,
            shown: RunNames::new(&state).shown(&id),
            join: JoinTarget { id, reading },
        });
    }
    if resolve::names_workflow(raw) {
        return Err(UsageError::new(format!(
            "`{raw}` names a workflow, not a run; name each run by its id or \
             <workflow>#<checkout> address (`rimz workflow list`)"
        ))
        .into());
    }
    let (state, id) = match resolve::select(raw, "show", globals, None, None, &[])? {
        resolve::Selection::Run(run) => (run.state, run.record.id),
        resolve::Selection::Other(other) => (other.state, other.id),
        resolve::Selection::Workflow(_) => unreachable!("an address selects one run"),
    };
    let reading = look(&id)?;
    Ok(Target {
        address: raw.to_owned(),
        shown: RunNames::new(&state).shown(&id),
        join: JoinTarget { id, reading },
    })
}

/// One look at a run's home; a home gone after it was found reads as a quiet ending.
fn look(id: &WorkflowRunId) -> Result<Reading> {
    match join::look(id)? {
        None => Ok(Reading::Ended(Ended::quiet())),
        Some(Ok(reading)) => Ok(reading),
        Some(Err(unreadable)) => Err(refusal(*unreadable)),
    }
}

/// Why a home this RimZ cannot read refuses the wait.
fn refusal(unreadable: join::Unreadable) -> anyhow::Error {
    let id = unreadable.id.clone();
    anyhow::anyhow!(
        "{}",
        resolve::Other {
            state: unreadable.state,
            id: unreadable.id,
            found: unreadable.found,
        }
        .refusal()
        .unwrap_or_else(|| format!("this RimZ cannot read workflow run {id}"))
    )
}

/// Why a run waits on a person, worded from its record.
fn needs_you_why(record: &RunRecord, definition_dir: &std::path::Path) -> String {
    let spec = workflow::load(definition_dir).ok();
    workflow_view::detail(record, spec.as_ref(), jiff::Timestamp::now(), None)
}

fn line(target: &Target) -> String {
    let name = &target.shown;
    match &target.join.reading {
        Reading::Running { state, node } => match node {
            Some(node) => format!("{name}: still {} at {node}", state.as_str()),
            None => format!("{name}: still {}", state.as_str()),
        },
        Reading::NeedsYou {
            record,
            definition_dir,
        } => format!(
            "{name}: needs you: {}",
            render::one_line(&needs_you_why(record, definition_dir))
        ),
        Reading::Ended(ended) => {
            let mut line = format!(
                "{name}: {}",
                workflow_view::state_word(ended.state, ended.ending.as_ref())
            );
            if let Some(checkout) = &ended.checkout {
                line.push_str(&format!(" · checkout {}", checkout.display()));
            }
            if let Some(notes) = &ended.notes {
                line.push_str(&format!(" · notes {}", notes.display()));
            }
            line
        }
    }
}

fn report(targets: &[Target], json: bool, stop: i32) -> Result<()> {
    if json {
        let entries = targets.iter().map(|target| entry(target, stop));
        return render::json_pretty(&entries.collect::<Vec<_>>());
    }
    let mut out = render::out();
    for target in targets {
        writeln!(out, "{}", line(target))?;
    }
    Ok(())
}

fn entry(target: &Target, stop: i32) -> serde_json::Value {
    let mut entry = serde_json::json!({ "id": target.join.id, "address": target.address });
    match &target.join.reading {
        Reading::Running { state, node } => {
            entry["outcome"] = serde_json::json!("running");
            entry["state"] = serde_json::json!(state);
            entry["node"] = serde_json::json!(node);
        }
        Reading::NeedsYou {
            record,
            definition_dir,
        } => {
            entry["outcome"] = serde_json::json!("needs_you");
            entry["reason"] = serde_json::json!(needs_you_why(record, definition_dir));
        }
        Reading::Ended(ended) => {
            entry["outcome"] = serde_json::json!("ended");
            entry["state"] = serde_json::json!(ended.state);
            entry["ending"] = serde_json::json!(ended.ending);
            entry["checkout"] = serde_json::json!(ended.checkout);
            entry["notes"] = serde_json::json!(ended.notes);
        }
    }
    entry["exit"] = serde_json::json!(exit_of(target, stop));
    entry
}
