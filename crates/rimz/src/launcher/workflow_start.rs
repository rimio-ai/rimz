//! The job of `rimz workflow run` up to the created run, shared by the command and the MCP
//! `workflow` tool. Warnings come back as values, so each caller presents them its own way.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use super::Interaction;
use crate::config::MachineConfig;
use crate::disk::paths;
use crate::harness::workflow::{self, Deployed};
use crate::ids::WorkflowRunId;
use crate::store::workflow::{self as records, RunRecord};
use crate::{ResolvedWorkspace, Store};

/// The definitions a workflow's agent steps resolve against.
pub fn definitions() -> crate::config::definitions::LoadedDefinitions {
    let machine = MachineConfig::load_lenient();
    crate::config::definitions::load(
        &paths::agents_home(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &MachineConfig::config_path(),
        &machine.tiers,
        machine.harness.auto_compact_default(),
    )
}

/// The workflows deployed in the agents home and its installed packs.
pub fn deployed() -> Result<Deployed> {
    let home = paths::agents_home();
    Deployed::load(&home).map_err(|error| {
        anyhow!(
            "reading {}: {error}",
            paths::workflows_dir_under(&home).display()
        )
    })
}

/// Where a workflow target leads: a target of one bare path component is a deployed name,
/// `None` when nothing claims it, and any other target is the directory it spells.
pub fn definition_dir(target: &Path) -> Result<Option<PathBuf>> {
    let mut components = target.components();
    let name = match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(name)), None) => name,
        _ => return Ok(Some(target.to_owned())),
    };
    let Some(name) = name.to_str() else {
        return Ok(None);
    };
    let deployed = deployed()?;
    let found = deployed
        .find(name)
        .map_err(|conflict| anyhow!("{conflict}"))?;
    Ok(found.map(Path::to_owned))
}

/// The refusal for a name no deployed workflow holds.
pub fn not_deployed(name: &Path) -> String {
    let home = paths::workflows_dir_under(&paths::agents_home());
    format!(
        "{}: no workflow of that name in {} or an installed pack",
        name.display(),
        home.display()
    )
}

/// Validate the definition at `dir`: refused with every error, else its warnings.
pub fn validated(dir: &Path, project_root: Option<&Path>) -> Result<Vec<String>> {
    let report = workflow::validate(dir, &definitions(), project_root);
    if !report.errors.is_empty() {
        bail!(
            "{}",
            report
                .errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(report.warnings.into_iter().map(|w| w.message).collect())
}

/// Refuses before `action` changes anything when the detached engine could not open the
/// home-wide start lock its first launch takes. It checks RimZ's own needs only.
pub fn refuse_unwritable_home(action: &str) -> Result<()> {
    crate::harness::throttle::probe_queue_lock().map_err(|error| unwritable_home(error, action))
}

fn unwritable_home(error: crate::disk::lock::LockErr, action: &str) -> anyhow::Error {
    let error = anyhow::Error::from(error);
    if crate::harness::home_write::is_read_only(&error) {
        return crate::harness::home_write::hint(error, "rimz workflow", Some(action));
    }
    error.context(format!(
        "nothing was {action}: the detached workflow engine needs this lock"
    ))
}

/// Who starts a run from this process.
pub struct StartedHere {
    pub starting: workflow::Starting,
    /// An agent resolves under a hosted variable with no run to start as: the start owes it
    /// nothing, but it is still the agent's to refuse.
    agent_without_run: bool,
}

/// Who starts a run from this process ([`workflow::starting`]): the run `RIMZ_WORKFLOW_RUN`
/// names, when this RimZ reads it, the calling agent, when one resolves, and whether a run's
/// command step or check spawned this process (`RIMZ_WORKFLOW_NODE`).
pub fn started_here(store: &Store) -> StartedHere {
    let enclosing = std::env::var("RIMZ_WORKFLOW_RUN")
        .ok()
        .and_then(|raw| raw.parse::<WorkflowRunId>().ok())
        .and_then(|id| found_run(&id))
        .and_then(|found| match found.class {
            records::RunClass::Current(record) => Some(*record),
            _ => None,
        });
    let snapshot = store.snapshot().ok();
    let agent = snapshot.as_ref().and_then(|snapshot| {
        crate::harness::ancestry::resolve_calling_agent(&snapshot.agents).ok()
    });
    let hosted = std::env::var_os("RIMZ_WORKFLOW_NODE").is_some();
    StartedHere {
        agent_without_run: hosted && enclosing.is_none() && agent.is_some(),
        starting: workflow::starting(enclosing.as_ref(), agent, hosted),
    }
}

fn found_run(id: &WorkflowRunId) -> Option<records::FoundRun> {
    let (_, found) = records::find_under(&paths::rimz_home(), id).ok()??;
    Some(found)
}

/// Whether an agent stands at the root of `starting`'s launch path: it starts the run itself, or
/// it started the run whose step or script starts this one, at any depth. A starter run whose
/// record cannot be read could be an agent's, so it counts as one.
fn agent_started(starting: &workflow::Starting) -> bool {
    let mut starter = starting.starter.clone();
    for _ in 0..=starting.depth {
        let run = match starter {
            None => return false,
            Some(records::RunStarter::Agent { .. }) => return true,
            Some(records::RunStarter::Run { run, .. }) => run,
        };
        starter = match found_run(&run).map(|found| found.class) {
            Some(records::RunClass::Current(record)) => record.starter,
            Some(records::RunClass::Stranded { header, .. } | records::RunClass::Ended(header)) => {
                header.starter
            }
            _ => return true,
        };
    }
    true
}

/// What a start runs as: a loop task's own fire, or whoever called from this process.
pub enum StartOrigin {
    Task(workflow::TaskStart),
    Here(StartedHere),
}

/// One start of the deployed or named definition at `dir`, already validated.
pub struct WorkflowStart<'a> {
    pub name: &'a Path,
    pub dir: PathBuf,
    pub worktree: Option<&'a str>,
    /// How a refusal names `worktree` to this caller: the command's flag or the tool's argument.
    pub worktree_arg: &'a str,
    pub prompt: Option<String>,
    pub args: workflow::StartArgs,
    pub origin: StartOrigin,
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    pub machine: &'a MachineConfig,
}

/// Create the run, after every refusal the start owes: scope, depth, what an agent or task may
/// start, overlap, trust, and a writable home. `None` when the caller declined the checkout.
/// An overlap is a [`workflow::WorkflowErr::Exists`] the caller can read from the error.
pub fn start(
    request: WorkflowStart<'_>,
    interaction: &mut dyn Interaction,
) -> Result<Option<(RunRecord, workflow::Advance)>> {
    let WorkflowStart {
        name,
        dir,
        worktree,
        worktree_arg,
        prompt,
        mut args,
        origin,
        workspace,
        store,
        machine,
    } = request;
    let definition = workflow::load(&dir)?;
    if worktree.is_some() && definition.scope() == workflow::ScopeKind::Project {
        bail!(
            "{} has scope = project, so it runs at {}; start it without {worktree_arg}",
            workflow::directory_name(&dir).unwrap_or_else(|| dir.display().to_string()),
            workspace.project_root.display()
        );
    }
    let deployed = deployed()?;
    // A task's own run starts nothing on a launch path; any other start is placed on one and
    // bounded before anything is created. A task's run and an agent's start only what is
    // deployed, read from the claim's own path so no link the caller made is recorded.
    let (dir, task) = match origin {
        StartOrigin::Task(task) => {
            let claimed = deployed.claim(&dir).ok_or_else(|| {
                anyhow!(
                    "{}: a loop task starts only a workflow in {} or an installed pack",
                    name.display(),
                    deployed.home().display()
                )
            })?;
            (claimed, Some(task))
        }
        StartOrigin::Here(here) => {
            args.starting = here.starting;
            args.starting.admit(machine.agents.max_workflow_depth)?;
            if !here.agent_without_run && !agent_started(&args.starting) {
                (dir, None)
            } else {
                let claimed = deployed.claim(&dir).ok_or_else(|| {
                    anyhow!(
                        "{}: an agent starts only a workflow in {} or an installed pack; run \
                         this directory from your own shell, or move it there or install its \
                         pack",
                        name.display(),
                        deployed.home().display()
                    )
                })?;
                (claimed, None)
            }
        }
    };
    let env_fire = crate::harness::loop_task::read_fire_env().map(|fire| fire.fire);
    let fire = task
        .as_ref()
        .map(|task| task.loop_fire.clone())
        .or(env_fire.clone());
    // A workflow task's fire names its run as the task's own. Any other run started under a
    // loop fire carries the fire: listed and stopped by its task, reported to nobody.
    let manual = task.as_ref().is_some_and(|task| task.manual);
    let owned = task.is_some();
    let source = match task {
        Some(task) => workflow::DefinitionSource::Deployed {
            dir: &dir,
            task: Some(task.task),
            loop_fire: Some(task.loop_fire),
            fire: Some(task.fire),
            fire_facts: Some(task.fire_facts),
        },
        None => workflow::DefinitionSource::Deployed {
            dir: &dir,
            task: None,
            loop_fire: env_fire,
            fire: None,
            fire_facts: None,
        },
    };
    let planned = match worktree {
        Some(name) => crate::worktree::worktree_path(
            workspace.launch_repo_root(),
            &machine.agents.worktree,
            name,
        )?,
        None => workspace.worktree_root.clone(),
    };
    let roots = |checkout: PathBuf| workflow::ScopeRoots {
        checkout,
        project: workspace.project_root.clone(),
    };
    // A slot a live run holds is named before the trust read, as `create` names it.
    workflow::refuse_overlap(store.paths(), &source, roots(planned.clone()), args.once)?;
    refuse_trust_problems(
        workspace,
        store,
        machine,
        &definition,
        roots(planned).path(definition.scope()),
        &args,
        owned,
    )?;
    refuse_unwritable_home("started")?;
    let scope = match worktree {
        Some(name) => {
            if name.trim().is_empty() {
                bail!("{worktree_arg} takes a worktree name");
            }
            match named_checkout(workspace, store, machine, name, fire.as_ref(), interaction)? {
                Some(scope) => scope,
                None => return Ok(None),
            }
        }
        None => workspace.worktree_root.clone(),
    };
    let created = workflow::create(
        store.paths(),
        source,
        roots(scope),
        || Ok(prompt),
        manual,
        args,
    )?;
    Ok(Some(created))
}

/// Refuses the start, before anything is created, when the launcher's trust read refuses an
/// agent step or judge the run could reach (with `--once`, the tried node and its rest check),
/// listing each problem once with its fix and the nodes it blocks.
fn refuse_trust_problems(
    workspace: &ResolvedWorkspace,
    store: &Store,
    machine: &MachineConfig,
    definition: &workflow::Definition,
    scope: &Path,
    args: &workflow::StartArgs,
    task_run: bool,
) -> Result<()> {
    let tried = args
        .once
        .then(|| args.at.as_deref().unwrap_or(definition.start()));
    let problems = super::trust::workflow_start_problems(
        workspace, store, machine, definition, scope, tried, task_run,
    )?;
    if problems.is_empty() {
        return Ok(());
    }
    let mut message = String::from("workflow start refused; nothing was created:");
    for (problem, nodes) in problems {
        message.push_str(&format!("\n- {problem} (blocks {})", nodes.join(", ")));
    }
    bail!(message)
}

/// The RimZ-owned worktree `-w <name>` names, created from the configured base when missing,
/// as an agent launch's `-w` creates it: in the room's repository, or after the caller confirms
/// the current one, and stamped with the loop fire the run starts under. `None` when the caller
/// declined another repository or a checkout RimZ does not own.
fn named_checkout(
    workspace: &ResolvedWorkspace,
    store: &Store,
    machine: &MachineConfig,
    name: &str,
    fire: Option<&crate::ids::FireId>,
    interaction: &mut dyn Interaction,
) -> Result<Option<PathBuf>> {
    if !super::checkout::confirm_cross_repo_worktree(workspace, interaction)? {
        return Ok(None);
    }
    let projection = store.runtime_projection(crate::RuntimeScope::Audit)?;
    crate::harness::lanes::admit_launch_worktree_name(
        workspace,
        &machine.agents.worktree,
        &projection.agents,
        Some(name),
    )?;
    let Some(checkout) = super::checkout::resolve_launch_checkout(
        workspace,
        machine,
        Some(name),
        None,
        None,
        crate::worktree::LaunchBase::Configured,
        interaction,
    )?
    else {
        return Ok(None);
    };
    if let (Some(fire), Some(_)) = (fire, &checkout.created)
        && let Err(error) = crate::worktree::stamp_loop_fire(&checkout.cwd, fire)
    {
        tracing::warn!(
            %fire,
            %error,
            path = %checkout.cwd.display(),
            "the run's tree carries no fire stamp"
        );
    }
    Ok(Some(checkout.cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwritable_home_adds_the_sandbox_remedy_only_for_a_read_only_filesystem() {
        for action in ["started", "resumed", "extended", "restarted"] {
            for kind in [
                std::io::ErrorKind::ReadOnlyFilesystem,
                std::io::ErrorKind::PermissionDenied,
            ] {
                let error = crate::disk::lock::LockErr::Open {
                    path: "rimz-home/loops/throttle.lock".into(),
                    source: std::io::Error::from(kind),
                };
                let report = format!("{:#}", unwritable_home(error, action));
                assert!(
                    report.contains(&format!("nothing was {action}: ")),
                    "{report}"
                );
                assert!(report.contains("throttle.lock"), "{report}");
                let read_only = kind == std::io::ErrorKind::ReadOnlyFilesystem;
                for text in [
                    "this command cannot write the RimZ home",
                    "Run `rimz workflow …` as its own command",
                ] {
                    assert_eq!(report.contains(text), read_only, "{report}");
                }
                assert_eq!(
                    report.contains("the detached workflow engine needs this lock"),
                    !read_only,
                    "{report}"
                );
            }
        }
    }
}
