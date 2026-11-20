//! `rimz workflow wait`: block until named workflow runs end, or one needs a person.

use std::time::{Duration, Instant};

use rimz::store::workflow::{RunClass, RunEnding, RunState};

use super::*;
use crate::cli::render::announce::{MESSAGE_STOP_EXIT, write_message_stop};
use crate::cli::usage::UsageError;
use rimz::message::announce::WaitCaller;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The exit of a wait that returned because an awaited run needs a person.
const NEEDS_YOU_EXIT: i32 = 3;
const TIMED_OUT_EXIT: i32 = 124;

struct Target {
    id: WorkflowRunId,
    address: String,
    reading: Reading,
}

/// What one look at a run's home found.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reading {
    Running {
        state: RunState,
        node: Option<String>,
    },
    NeedsYou {
        why: String,
    },
    Ended(Ended),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ended {
    state: RunState,
    ending: Option<RunEnding>,
    checkout: Option<PathBuf>,
    notes: Option<PathBuf>,
    terminal_at: Option<jiff::Timestamp>,
}

impl Ended {
    /// A run whose home was deleted after the wait first found it: only a quiet task run's
    /// home is deleted at its end, and only after it ended done.
    fn quiet() -> Self {
        Self {
            state: RunState::Done,
            ending: None,
            checkout: None,
            notes: None,
            terminal_at: None,
        }
    }

    fn exit(&self) -> i32 {
        i32::from(self.state != RunState::Done)
    }
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
    let started = Instant::now();
    loop {
        for target in &mut targets {
            if !matches!(target.reading, Reading::Ended(_)) {
                target.reading =
                    look(&target.id)?.unwrap_or_else(|| Reading::Ended(Ended::quiet()));
            }
        }
        let first_ended = targets
            .iter()
            .filter_map(|target| match &target.reading {
                Reading::Ended(ended) => Some((target, ended.terminal_at)),
                _ => None,
            })
            .min_by_key(|(_, terminal_at)| (terminal_at.is_none(), *terminal_at));
        if any && let Some((target, _)) = first_ended {
            report(std::slice::from_ref(target), json, 0)?;
            std::process::exit(exit_of(target, 0));
        }
        if targets
            .iter()
            .all(|target| matches!(target.reading, Reading::Ended(_)))
        {
            report(&targets, json, 0)?;
            std::process::exit(
                targets
                    .iter()
                    .map(|target| exit_of(target, 0))
                    .max()
                    .unwrap_or(0),
            );
        }
        if targets
            .iter()
            .any(|target| matches!(target.reading, Reading::NeedsYou { .. }))
        {
            report(&targets, json, NEEDS_YOU_EXIT)?;
            std::process::exit(NEEDS_YOU_EXIT);
        }
        if let Some(ctx) = &ctx
            && let Some(announcement) = caller.queued(&ctx.store, &ctx.workspace.session_name)
        {
            report(&targets, json, MESSAGE_STOP_EXIT)?;
            write_message_stop(&mut render::err(), &announcement)?;
            std::process::exit(MESSAGE_STOP_EXIT);
        }
        if let Some(timeout) = timeout
            && started.elapsed() >= timeout
        {
            report(&targets, json, TIMED_OUT_EXIT)?;
            writeln!(
                render::err(),
                "timed out after {}; the runs still going are left running",
                render::age_label(timeout.as_secs())
            )?;
            std::process::exit(TIMED_OUT_EXIT);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// A target's exit under a wait that stops with `stop`: a run still going carries the stop's.
fn exit_of(target: &Target, stop: i32) -> i32 {
    match &target.reading {
        Reading::Ended(ended) => ended.exit(),
        Reading::NeedsYou { .. } => NEEDS_YOU_EXIT,
        Reading::Running { .. } => stop,
    }
}

/// The run a reference names, by id or address. An id found in no home is a usage error, and a
/// bare workflow name names no one run.
fn target(raw: &str, globals: &super::super::GlobalFlags) -> Result<Target> {
    if let Ok(id) = raw.parse::<WorkflowRunId>() {
        if records::find_under(&paths::rimz_home(), &id)?.is_none() {
            return Err(UsageError::new(format!("unknown workflow run {id}")).into());
        }
        let reading = look(&id)?.unwrap_or_else(|| Reading::Ended(Ended::quiet()));
        let address = match records::find_under(&paths::rimz_home(), &id)? {
            Some((_, found)) => match found.class {
                RunClass::Current(record) => record.address().to_string(),
                _ => id.to_string(),
            },
            None => id.to_string(),
        };
        return Ok(Target {
            id,
            address,
            reading,
        });
    }
    if resolve::names_workflow(raw) {
        return Err(UsageError::new(format!(
            "`{raw}` names a workflow, not a run; name each run by its id or \
             <workflow>#<checkout> address (`rimz workflow list`)"
        ))
        .into());
    }
    let id = match resolve::select(raw, "show", globals, None, None, &[])? {
        resolve::Selection::Run(run) => run.record.id,
        resolve::Selection::Other(other) => other.id,
        resolve::Selection::Workflow(_) => unreachable!("an address selects one run"),
    };
    let reading = look(&id)?.unwrap_or_else(|| Reading::Ended(Ended::quiet()));
    Ok(Target {
        id,
        address: raw.to_owned(),
        reading,
    })
}

/// One look at a run's home, or none once the home is gone. A record this RimZ cannot read
/// refuses rather than guess an ending; a stranded run waits for the pass that ends it.
fn look(id: &WorkflowRunId) -> Result<Option<Reading>> {
    let Some((state, found)) = records::find_under(&paths::rimz_home(), id)? else {
        return Ok(None);
    };
    let notes = |relative: Option<&PathBuf>| {
        relative.map(|relative| state.notes_run(paths::Class::AutomationNotes, relative))
    };
    Ok(Some(match found.class {
        RunClass::Current(record) if found.finished || record.state.is_terminal() => {
            Reading::Ended(Ended {
                state: record.state,
                ending: record
                    .outcome
                    .as_ref()
                    .map(|outcome| outcome.ending.clone()),
                checkout: Some(record.scope.path.clone()),
                notes: notes(record.notes.as_ref()),
                terminal_at: record.terminal_at,
            })
        }
        RunClass::Current(record) if workflow_view::needs_you(&record) => {
            let spec = workflow::load(&record.definition_dir(&found.paths)).ok();
            Reading::NeedsYou {
                why: workflow_view::detail(&record, spec.as_ref(), jiff::Timestamp::now(), None),
            }
        }
        RunClass::Current(record) => Reading::Running {
            state: record.state,
            node: Some(record.node.clone()),
        },
        RunClass::Ended(header) => {
            let Some(state) = header.state.filter(|state| state.is_terminal()) else {
                bail!("workflow run {id} has ended, and this RimZ cannot read how");
            };
            Reading::Ended(Ended {
                state,
                ending: header.ending.clone(),
                checkout: header.scope_path.clone(),
                notes: notes(header.notes.as_ref()),
                terminal_at: header.terminal_at,
            })
        }
        RunClass::Stranded { header, .. } => Reading::Running {
            state: header.state.unwrap_or(RunState::Active),
            node: header.node.clone(),
        },
        RunClass::Recordless => Reading::Running {
            state: RunState::Active,
            node: None,
        },
        RunClass::Newer { .. } | RunClass::NotObject => {
            bail!(
                "{}",
                resolve::Other {
                    state,
                    id: id.clone(),
                    found,
                }
                .refusal()
                .unwrap_or_else(|| format!("this RimZ cannot read workflow run {id}"))
            );
        }
    }))
}

fn line(target: &Target) -> String {
    let name = if target.address == target.id.as_str() {
        target.id.to_string()
    } else {
        format!("{} ({})", target.address, target.id)
    };
    match &target.reading {
        Reading::Running { state, node } => match node {
            Some(node) => format!("{name}: still {} at {node}", state.as_str()),
            None => format!("{name}: still {}", state.as_str()),
        },
        Reading::NeedsYou { why } => format!("{name}: needs you: {}", render::one_line(why)),
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
    let mut entry = serde_json::json!({ "id": target.id, "address": target.address });
    match &target.reading {
        Reading::Running { state, node } => {
            entry["outcome"] = serde_json::json!("running");
            entry["state"] = serde_json::json!(state);
            entry["node"] = serde_json::json!(node);
        }
        Reading::NeedsYou { why } => {
            entry["outcome"] = serde_json::json!("needs_you");
            entry["reason"] = serde_json::json!(why);
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
