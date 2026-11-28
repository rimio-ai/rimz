//! `schedule`: the job of `rimz loop add` for an agent, a team, a workflow, or a command,
//! booked in the caller's room through the write the command makes. The command's check of a
//! fire's launch parses with the binary's root parser, out of reach here, so this refuses each
//! argument the stored launch would misparse, and resolves each target the way that check
//! resolves it.

use std::path::{Path, PathBuf};

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
use crate::harness::block::{Schedule, ScheduleOverlap, ScheduleTrigger};
use crate::harness::schedule::arm::task_label;
use crate::harness::schedule::attention::when_scope;
use crate::harness::schedule::catalog::TaskCatalog;
use crate::harness::schedule::wait::{WhenPlace, WindowAccount, at_refusal, read_when};
use crate::harness::schedule::{self, TaskShape, shell_task};
use crate::harness::spec::Cell;
use crate::harness::workflow;
use crate::launcher::LaunchOverrides;
use crate::launcher::workflow_start;
use crate::mcp::catalog::{Booking, ScheduleTarget, WorkflowArgs};
use crate::mcp::{Answer, Failure};

/// What a booker hears of a fire: nothing delivers one a message.
const REPORT: &str = "no message when it fires; `tasks` lists it and `stop` removes it";

/// What a fire launches, as its stored row holds it.
struct Launch {
    kind: LaunchKind,
    target: String,
    args: Vec<String>,
    worktree: Option<String>,
    /// The directory a command's `cwd` names; the caller's checkout when `None`.
    dir: Option<PathBuf>,
    /// A command's time limit and label.
    timeout: Option<String>,
    label: Option<String>,
    /// The agent kinds the target routes to; empty for a team or a workflow.
    kinds: Vec<String>,
    warnings: Vec<String>,
}

/// Book `booking` in the caller's room, owned by the caller.
pub(super) fn book(tools: &Tools, booking: &Booking) -> Result<Answer, Failure> {
    let args = &booking.schedule;
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
    let launch = launch(tools, &room, &machine, booking)?;
    let mut entry = TaskEntry {
        kind: Some(launch.kind),
        target: (launch.kind != LaunchKind::Shell).then(|| launch.target.clone()),
        args: launch.args.clone(),
        worktree: launch.worktree.clone(),
        dir: Some(match &launch.dir {
            Some(dir) => dir.clone(),
            None => room.workspace.worktree_root.clone(),
        }),
        timeout: launch.timeout.clone(),
        label: launch.label.clone(),
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
    set_trigger(&mut entry, args, &machine)?;
    if let Some(clauses) = &entry.when {
        let scope = when_scope(&entry);
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
    shape
        .trigger()
        .as_ref()
        .map_err(|err| Failure::new(err.to_string()))?;
    shape
        .action()
        .map_err(|err| Failure::new(err.to_string()))?;
    entry.booked_by = booked_by;
    let _held = TaskCatalog::lock_edits()?;
    let catalog = TaskCatalog::load(Some(&root))?;
    refuse_held_name(&catalog, &args.name, &entry.booked_by)?;
    catalog.replace_machine(&args.name, &entry)?;
    let mut warnings = launch.warnings;
    warnings.extend(idle_warning(booking));
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

fn launch(
    tools: &Tools,
    room: &Room,
    machine: &MachineConfig,
    booking: &Booking,
) -> Result<Launch, Failure> {
    let args = &booking.schedule;
    let root = &room.workspace.project_root;
    if let Some(worktree) = &args.worktree {
        crate::worktree::parse_requested_name(worktree).map_err(anyhow::Error::from)?;
    }
    // A fire meeting the closed worktree an earlier fire's agent or team left launches fresh
    // there; without `--fresh` an unattended launch refuses it. Emit it once even if the caller
    // also set `arguments.fresh`.
    let fresh = args.worktree.as_ref().map(|_| "--fresh".to_owned());
    match &booking.target {
        ScheduleTarget::Agent(agent) => {
            let profile = agent.profile.trim();
            let effective = effective::load(machine, root).map_err(anyhow::Error::from)?;
            if effective.teams.0.contains_key(profile) {
                return Err(Failure::new(format!(
                    "`arguments.profile` `{profile}` is a team; book it with `tool` `team`"
                )));
            }
            let agent_override =
                crate::harness::plan::normalized_preset_value(agent.agent.as_deref());
            let lane = crate::harness::lanes::directory_channel(root, root);
            let snapshot = lane
                .as_ref()
                .map(|_| room.store.snapshot_cached())
                .transpose()
                .map_err(anyhow::Error::from)?;
            let overrides = LaunchOverrides {
                agent: agent_override.clone(),
                ..LaunchOverrides::default()
            };
            let finalized = crate::launcher::resolve::resolve_finalized_layout(
                snapshot.as_ref(),
                machine,
                &effective,
                ProfileScope::Agents,
                Some(profile),
                Some(&agent.prompt),
                &overrides,
                None,
                None,
                lane.as_ref(),
                false,
                None,
            )?;
            let cells: Vec<&Cell> = finalized
                .resolved
                .layout
                .columns
                .iter()
                .flat_map(|column| &column.rows)
                .collect();
            match cells.as_slice() {
                [Cell::Agent(_)] => {}
                [_] => {
                    return Err(Failure::new(format!(
                        "`arguments.profile` `{profile}` names a command, not an agent; name an \
                         agent profile"
                    )));
                }
                _ => {
                    return Err(Failure::new(format!(
                        "`arguments.profile` `{profile}` names more than one cell; name one agent \
                         profile"
                    )));
                }
            }
            if let Some(name) = agent.name.as_deref() {
                crate::harness::plan::validate_agent_name(name)
                    .map_err(|err| Failure::new(format!("`arguments.name`: {err}")))?;
            }
            let from_pr = parse_pr_flag(agent.from_pr.as_deref())?;
            if args.repeats() && from_pr.is_some() && finalized.resolved.team_name.is_some() {
                return Err(Failure::new(
                    "`arguments.from_pr`: a repeating booking of a team seat cannot use \
                     `from_pr`; set top-level `worktree` or use a one-shot trigger",
                ));
            }
            let cwd = agent
                .cwd
                .as_deref()
                .map(|cwd| {
                    host_dir(tools, room, cwd)?
                        .to_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Failure::new("`arguments.cwd` is not valid UTF-8"))
                })
                .transpose()?;
            let kinds = crate::launcher::resolve::routable_kinds(
                snapshot.as_ref(),
                machine,
                &effective,
                ProfileScope::Agents,
                Some(profile),
                Some(&agent.prompt),
                &overrides,
                None,
                None,
                lane.as_ref(),
                agent.name.is_some(),
            )?;
            let mut tail = Vec::new();
            tail.extend(fresh);
            if let Some(description) = &agent.description {
                tail.push(format!("--description={description}"));
            }
            if let Some(name) = &agent.name {
                tail.push(format!("--name={name}"));
            }
            if let Some(agent_override) = &agent_override {
                tail.push(format!("--agent={agent_override}"));
            }
            if let Some(from_pr) = &from_pr {
                tail.push(format!("--from-pr={from_pr}"));
            }
            if let Some(cwd) = &cwd {
                tail.push(format!("--cwd={cwd}"));
            }
            tail.push(agent.prompt.clone());
            Ok(Launch {
                kind: LaunchKind::Agent,
                target: profile.to_owned(),
                args: tail,
                worktree: args.worktree.clone(),
                dir: None,
                timeout: None,
                label: None,
                kinds,
                warnings: Vec::new(),
            })
        }
        ScheduleTarget::Team(team) => {
            let team_name = team.team.trim();
            // A team's fire loads the machine config strictly, where this tool reads it
            // leniently.
            let strict = MachineConfig::load().map_err(|err| {
                Failure::new(format!(
                    "the machine config does not load, so a team's fire cannot launch: {err:#}"
                ))
            })?;
            let effective = effective::load(&strict, root).map_err(anyhow::Error::from)?;
            effective.block_set_failure().map_err(anyhow::Error::from)?;
            if !effective.teams.0.contains_key(team_name) {
                if let Some(detail) = effective.definition_failure_for(team_name) {
                    return Err(Failure::new(detail));
                }
                return Err(Failure::new(format!("unknown team `{team_name}`")));
            }
            let from_pr = parse_pr_flag(team.from_pr.as_deref())?;
            let mut tail = Vec::new();
            tail.extend(fresh);
            if let Some(description) = &team.description {
                tail.push(format!("--description={description}"));
            }
            if let Some(from_pr) = &from_pr {
                tail.push(format!("--from-pr={from_pr}"));
            }
            tail.push(team.prompt.clone());
            Ok(Launch {
                kind: LaunchKind::Team,
                target: team_name.to_owned(),
                args: tail,
                worktree: args.worktree.clone(),
                dir: None,
                timeout: None,
                label: None,
                kinds: Vec::new(),
                warnings: Vec::new(),
            })
        }
        ScheduleTarget::Workflow(run) => workflow_launch(root, args.worktree.as_deref(), run),
        ScheduleTarget::Shell(shell) => Ok(Launch {
            kind: LaunchKind::Shell,
            target: String::new(),
            args: vec!["sh".to_owned(), "-c".to_owned(), shell.command.clone()],
            worktree: args.worktree.clone(),
            dir: shell
                .cwd
                .as_deref()
                .map(|cwd| host_dir(tools, room, cwd))
                .transpose()?,
            timeout: Some(shell.timeout.as_deref().map_or_else(
                || shell_task::default_timeout(machine),
                |timeout| timeout.trim().to_owned(),
            )),
            label: shell
                .label
                .as_deref()
                .map(|raw| {
                    task_label(raw).map_err(|err| Failure::new(format!("`arguments.label`: {err}")))
                })
                .transpose()?,
            kinds: Vec::new(),
            warnings: Vec::new(),
        }),
    }
}

/// The host directory `cwd` names from the caller's, where a fire, which runs outside the
/// caller's sandbox, finds it.
fn host_dir(tools: &Tools, room: &Room, cwd: &Path) -> Result<PathBuf, Failure> {
    let resolved = tools.dir(Some(cwd))?;
    Ok(
        crate::launcher::checkout::resolve_launch_cwd(Some(&resolved), room.store.paths())?
            .unwrap_or(resolved),
    )
}

fn parse_pr_flag(raw: Option<&str>) -> Result<Option<String>, Failure> {
    raw.map(|raw| {
        crate::forge::parse(raw)
            .map(|_| raw.trim().to_owned())
            .map_err(|err| Failure::new(format!("`arguments.from_pr`: {err}")))
    })
    .transpose()
}

fn workflow_launch(
    root: &Path,
    worktree: Option<&str>,
    run: &WorkflowArgs,
) -> Result<Launch, Failure> {
    let wf_name = run.name.as_str();
    let name = Path::new(wf_name);
    let worktree = worktree.map(str::to_owned);
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
            "workflow `{wf_name}` has scope = project, so it runs at the project root; remove \
             `worktree`"
        )));
    }
    parse_when("arguments.deadline", run.deadline.as_deref())?;
    parse_when("arguments.give_up", run.give_up.as_deref())?;
    let mut tail = Vec::new();
    for (flag, value) in [
        ("--prompt", &run.prompt),
        ("--deadline", &run.deadline),
        ("--give-up", &run.give_up),
    ] {
        if let Some(value) = value {
            tail.push(format!("{flag}={value}"));
        }
    }
    if run.skip_capacity {
        tail.push("--skip-capacity".to_owned());
    }
    Ok(Launch {
        kind: LaunchKind::Workflow,
        target: wf_name.to_owned(),
        args: tail,
        worktree,
        dir: None,
        timeout: None,
        label: None,
        kinds: Vec::new(),
        warnings,
    })
}

fn set_trigger(
    entry: &mut TaskEntry,
    args: &Schedule,
    machine: &MachineConfig,
) -> Result<(), Failure> {
    let now = Timestamp::now().to_zoned_datetime(machine.time_zone());
    match args.trigger {
        ScheduleTrigger::At => {
            let fire_at = schedule::resolve_once_at(&args.name, &args.value, &now)
                .map_err(|err| at_refusal(err, "`at`"))?;
            entry.fire_at = Some(fire_at);
        }
        ScheduleTrigger::Cron => entry.cron = Some(args.value.clone()),
        ScheduleTrigger::When => {
            entry.when = Some(vec![args.value.clone()]);
            entry.hold.clone_from(&args.hold);
        }
    }
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
fn idle_warning(booking: &Booking) -> Option<String> {
    if !booking.schedule.repeats() || booking.schedule.overlap.is_some() {
        return None;
    }
    let choices = match booking.target {
        ScheduleTarget::Team(_) => "`replace`",
        ScheduleTarget::Agent(_) => "`replace` or `allow`",
        ScheduleTarget::Workflow(_) | ScheduleTarget::Shell(_) => return None,
    };
    Some(format!(
        "note: an agent left idle holds later fires; say in `arguments.prompt` when it ends its \
         session, or set `overlap` to {choices}"
    ))
}
