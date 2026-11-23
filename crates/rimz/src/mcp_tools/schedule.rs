//! `schedule`: the job of `rimz loop add` for an agent, a team, or a workflow, booked in the
//! caller's room through the write the command makes. The command's check of a fire's launch
//! parses with the binary's root parser, out of reach here, so this refuses each argument the
//! stored launch would misparse, and resolves each target the way that check resolves it.

use std::path::Path;

use jiff::Timestamp;

use super::Tools;
use super::room::Room;
use super::start::{Start, TaskKind};
use super::workflow::parse_when;
use crate::agents::LaunchedBy;
use crate::config::effective::{self, ProfileScope};
use crate::config::{LaunchKind, MachineConfig, OverlapPolicy, TaskEntry};
use crate::harness::ancestry::{
    LaunchAncestryError, booking_chain, resolve_calling_agent, resolve_launch_ancestry,
};
use crate::harness::plan::{NotOneAgent, ResolvedSingleAgentLaunch};
use crate::harness::schedule::catalog::TaskCatalog;
use crate::harness::schedule::wait::{WhenPlace, WindowAccount, at_refusal, read_when};
use crate::harness::schedule::{self, ScheduleErr, TaskShape};
use crate::harness::signal::SelectorErr;
use crate::harness::workflow;
use crate::launcher::LaunchOverrides;
use crate::launcher::workflow_start;
use crate::mcp::catalog::{ScheduleArgs, ScheduleOverlap, ScheduleTrigger, WorkflowArgs};
use crate::mcp::{Answer, Failure};

/// What a booker hears of a fire: nothing delivers one a message.
const REPORT: &str = "no message when it fires; `tasks` lists it and `stop` removes it";

/// What a fire launches, as its stored row holds it.
struct Launch {
    kind: LaunchKind,
    target: String,
    args: Vec<String>,
    worktree: Option<String>,
    /// The agent kinds the target routes to; empty for a team or a workflow.
    kinds: Vec<String>,
    warnings: Vec<String>,
}

/// Book `args` in the caller's room, owned by the caller.
pub(super) fn book(tools: &Tools, args: &ScheduleArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.dir(None)?)?;
    let rows = room
        .store
        .runtime_projection(crate::RuntimeScope::Audit)
        .map_err(anyhow::Error::from)?;
    let booker = resolve_calling_agent(&rows.agents).map_err(|_| {
        Failure::new("booking a schedule is only available to an agent RimZ can identify")
            .with_hint("serve this tool to an agent RimZ launched in this room")
    })?;
    let machine = MachineConfig::load_lenient();
    refuse_chain_limit(booker, machine.agents.max_chain_length)?;
    let booked_by = booking_chain(&rows.agents, booker);
    crate::harness::name::validate_name(&args.name).map_err(anyhow::Error::from)?;
    let root = room.workspace.project_root.clone();
    let launch = launch(&room, &machine, args)?;
    let mut entry = TaskEntry {
        kind: Some(launch.kind),
        target: Some(launch.target.clone()),
        args: launch.args.clone(),
        worktree: launch.worktree.clone(),
        max_strikes: args.max_strikes,
        overlap: args.overlap.map(|overlap| match overlap {
            ScheduleOverlap::Skip => OverlapPolicy::Skip,
            ScheduleOverlap::Coalesce => OverlapPolicy::Coalesce,
            ScheduleOverlap::Queue => OverlapPolicy::Queue,
            ScheduleOverlap::Replace => OverlapPolicy::Replace,
            ScheduleOverlap::Allow => OverlapPolicy::Allow,
        }),
        root: root.clone(),
        repeat: args.repeat,
        ..TaskEntry::default()
    };
    set_trigger(&mut entry, &args.trigger, &args.name, &machine)?;
    if let Some(clauses) = &entry.when {
        let scope = entry.run_dir();
        let place = WhenPlace {
            root: &root,
            scope: Some(&scope),
            sidebar_fresh: crate::sidebar::fresh_sidebar_present,
        };
        let armed = read_when(clauses, "`when`", &place, || {
            window_account(&launch.kinds, &entry)
        })?;
        entry.provider = armed.provider;
    }
    let shape = TaskShape::compile(&args.name, &entry);
    shape.trigger().as_ref().map_err(|err| match err {
        ScheduleErr::Selector(SelectorErr::ObsoleteCiSignal { name }) => Failure::new(format!(
            "schedule `{name}`: ci.finished was replaced by ci.passed, ci.failed, or 'ci.*'; \
             leave `conclusion` out of `match`"
        )),
        err => Failure::new(err.to_string()),
    })?;
    shape
        .action()
        .map_err(|err| Failure::new(err.to_string()))?;
    entry.booked_by = booked_by;
    let _held = TaskCatalog::lock_edits()?;
    let catalog = TaskCatalog::load(Some(&root))?;
    refuse_held_name(&catalog, &args.name, &entry.booked_by)?;
    catalog.replace_machine(&args.name, &entry)?;
    let mut warnings = launch.warnings;
    warnings.extend(idle_warning(args));
    if launch.worktree.is_none() && room.workspace.worktree_root != root {
        warnings.push(format!(
            "runs at the project root {}, not {}; set `worktree` to run each fire in a \
             RimZ-owned worktree",
            root.display(),
            room.workspace.worktree_root.display()
        ));
    }
    Ok(Answer::data(&Start {
        warnings,
        ..Start::running(TaskKind::Schedule, &args.name, REPORT)
    }))
}

/// A fire resolves no caller, so a booking is refused where a direct launch from the booker
/// would be: past the chain limit, or from a child.
fn refuse_chain_limit(booker: &crate::agents::AgentState, max: u8) -> Result<(), Failure> {
    match resolve_launch_ancestry(Some(booker), false, max) {
        Ok(_) => Ok(()),
        Err(LaunchAncestryError::ChainExceeded { current, max }) => Err(Failure::new(format!(
            "booking refused: this agent is {current} launches deep in an agent chain, and a \
             fire's launch would exceed this workspace's maximum chain length of {max}; do not \
             retry this booking"
        ))),
        Err(_) => Err(Failure::new(
            "booking refused: a subagent cannot book a launch; do the work yourself and report \
             the result to your caller",
        )),
    }
}

/// A name is replaced only by the agent that booked it, matched by launch id, which a molt to
/// another provider keeps: a row with no booker, a project row, and a row loading skipped are
/// all someone else's.
fn refuse_held_name(
    catalog: &TaskCatalog,
    name: &str,
    booked_by: &[LaunchedBy],
) -> Result<(), Failure> {
    let mine = catalog.visible().get(name).is_none_or(|task| {
        let launch = |chain: &[LaunchedBy]| chain.first().map(|link| link.agent_id.clone());
        launch(&task.entry().booked_by) == launch(booked_by)
    });
    if mine && !catalog.skipped_machine_row(name) {
        return Ok(());
    }
    Err(Failure::new(format!(
        "a schedule named `{name}` exists that you did not book; choose another name"
    )))
}

fn launch(room: &Room, machine: &MachineConfig, args: &ScheduleArgs) -> Result<Launch, Failure> {
    let root = &room.workspace.project_root;
    if let Some(worktree) = &args.worktree {
        crate::worktree::parse_requested_name(worktree).map_err(anyhow::Error::from)?;
    }
    // A fire meeting the closed worktree an earlier fire's agent or team left launches fresh
    // there; without `--fresh` an unattended launch refuses it.
    let fresh = args.worktree.as_ref().map(|_| "--fresh".to_owned());
    match (&args.run.agent, &args.run.team, &args.run.workflow) {
        (Some(agent), _, _) => {
            ResolvedSingleAgentLaunch::for_booking(&agent.profile, &room.workspace).map_err(
                |err| match err.downcast_ref::<NotOneAgent>() {
                    Some(NotOneAgent::Cells(_)) => Failure::new(format!(
                        "`run.agent.profile` `{}` names more than one cell; name one agent profile",
                        agent.profile
                    )),
                    Some(NotOneAgent::Command(_)) => Failure::new(format!(
                        "`run.agent.profile` `{}` names a command, not an agent; name an agent \
                         profile",
                        agent.profile
                    )),
                    None => Failure::from(err),
                },
            )?;
            let effective = effective::load(machine, root).map_err(anyhow::Error::from)?;
            let lane = crate::harness::lanes::directory_channel(root, root);
            let snapshot = lane
                .as_ref()
                .map(|_| room.store.snapshot_cached())
                .transpose()
                .map_err(anyhow::Error::from)?;
            let kinds = crate::launcher::resolve::routable_kinds(
                snapshot.as_ref(),
                machine,
                &effective,
                ProfileScope::Agents,
                Some(&agent.profile),
                Some(&agent.prompt),
                &LaunchOverrides::default(),
                None,
                None,
                lane.as_ref(),
                false,
            )?;
            Ok(Launch {
                kind: LaunchKind::Agent,
                target: agent.profile.clone(),
                args: fresh.into_iter().chain([agent.prompt.clone()]).collect(),
                worktree: args.worktree.clone(),
                kinds,
                warnings: Vec::new(),
            })
        }
        (None, Some(team), _) => {
            // A team's fire loads the machine config strictly, where this tool reads it
            // leniently.
            let strict = MachineConfig::load().map_err(|err| {
                Failure::new(format!(
                    "the machine config does not load, so a team's fire cannot launch: {err:#}"
                ))
            })?;
            let effective = effective::load(&strict, root).map_err(anyhow::Error::from)?;
            if !effective.teams.0.contains_key(&team.team) {
                return Err(Failure::new(format!("unknown team `{}`", team.team)));
            }
            Ok(Launch {
                kind: LaunchKind::Team,
                target: team.team.clone(),
                args: fresh.into_iter().chain([team.goal.clone()]).collect(),
                worktree: args.worktree.clone(),
                kinds: Vec::new(),
                warnings: Vec::new(),
            })
        }
        (None, None, Some(run)) => workflow_launch(root, args.worktree.as_deref(), run),
        (None, None, None) => Err(Failure::new(
            "`run` takes exactly one of `agent`, `team`, or `workflow`",
        )),
    }
}

fn workflow_launch(
    root: &Path,
    worktree: Option<&str>,
    run: &WorkflowArgs,
) -> Result<Launch, Failure> {
    let name = Path::new(&run.name);
    let worktree = match (worktree, run.worktree.as_deref()) {
        (Some(outer), Some(inner)) if outer != inner => {
            return Err(Failure::new(
                "`worktree` and `run.workflow.worktree` differ; keep one",
            ));
        }
        (outer, inner) => outer.or(inner).map(str::to_owned),
    };
    let dir = workflow_start::definition_dir(name)?
        .ok_or_else(|| Failure::new(workflow_start::not_deployed(name)))?;
    let warnings = workflow_start::validated(&dir, Some(root))?
        .into_iter()
        .map(|warning| format!("warning: {warning}"))
        .collect();
    if worktree.is_some()
        && workflow::load(&dir).map_err(anyhow::Error::from)?.scope()
            == workflow::ScopeKind::Project
    {
        return Err(Failure::new(format!(
            "workflow `{}` has scope = project, so it runs at the project root; remove \
             `worktree`",
            run.name
        )));
    }
    parse_when("run.workflow.deadline", run.deadline.as_deref())?;
    parse_when("run.workflow.give_up", run.give_up.as_deref())?;
    let mut tail = Vec::new();
    for (flag, value) in [
        ("--prompt", &run.prompt),
        ("--deadline", &run.deadline),
        ("--give-up", &run.give_up),
    ] {
        if let Some(value) = value {
            tail.extend([flag.to_owned(), value.clone()]);
        }
    }
    if run.skip_capacity {
        tail.push("--skip-capacity".to_owned());
    }
    Ok(Launch {
        kind: LaunchKind::Workflow,
        target: run.name.clone(),
        args: tail,
        worktree,
        kinds: Vec::new(),
        warnings,
    })
}

fn set_trigger(
    entry: &mut TaskEntry,
    trigger: &ScheduleTrigger,
    name: &str,
    machine: &MachineConfig,
) -> Result<(), Failure> {
    let now = Timestamp::now().to_zoned_datetime(machine.time_zone());
    if let Some(at) = &trigger.at {
        let fire_at =
            schedule::resolve_once_at(name, at, &now).map_err(|err| at_refusal(err, "`at`"))?;
        entry.fire_at = Some(fire_at);
    }
    if let Some(after) = &trigger.after {
        let delay = schedule::parse_delay(after)
            .map_err(|err| Failure::new(format!("invalid in `{after}`: {err}")))?;
        let fire_at = schedule::delayed_instant(delay, now.timestamp()).map_err(|_| {
            Failure::new(format!(
                "invalid in `{after}`: lands past the latest time RimZ can schedule"
            ))
        })?;
        entry.fire_at = Some(fire_at);
    }
    entry.every.clone_from(&trigger.every);
    entry.cron.clone_from(&trigger.cron);
    entry.signal.clone_from(&trigger.signal);
    entry.matches = trigger
        .matches
        .clone()
        .filter(|matches| !matches.is_empty());
    entry.when = trigger.when.clone().map(|when| vec![when]);
    entry.hold.clone_from(&trigger.hold);
    Ok(())
}

/// The account a booking's window terms read: the room's, on the one kind an agent target
/// runs on.
fn window_account(kinds: &[String], entry: &TaskEntry) -> anyhow::Result<WindowAccount> {
    let [kind] = kinds else {
        anyhow::bail!("`when` reads a usage window, which needs an `agent` target on one provider");
    };
    let root = entry.resolved_root();
    let runtime = crate::RuntimePaths::for_project_root(&root)?;
    let login = crate::harness::task_login::task_login(
        &crate::store::writer::LaunchLogin::RoomDefault,
        kind,
        &runtime,
    )?
    .map(|login| login.key());
    Ok(WindowAccount {
        kind: Some(kind.clone()),
        login,
        root,
        runtime,
    })
}

/// The note `rimz loop add` owes a repeating agent or team booking with no overlap policy.
fn idle_warning(args: &ScheduleArgs) -> Option<String> {
    let repeats = args.repeat || args.trigger.every.is_some() || args.trigger.cron.is_some();
    if !repeats || args.overlap.is_some() || args.run.workflow.is_some() {
        return None;
    }
    let (handoff, choices) = if args.run.team.is_some() {
        ("`run.team.goal`", "`replace`")
    } else {
        ("`run.agent.prompt`", "`replace` or `allow`")
    };
    Some(format!(
        "note: an agent left idle holds later fires; say in {handoff} when it ends its session, \
         or set `overlap` to {choices}"
    ))
}
