//! Loop task catalog, compiled runtime shapes, source precedence, and coordinated mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use super::{
    ParsedTrigger, ScheduleErr, TaskAction, TaskActionErr, TaskShape,
    arming::{self, ArmState, Arming},
    config_edit, instances, strikes,
    task_record::{Binding, Scope},
    task_record_store,
};
use crate::Store;
use crate::config::{LoopConfig, MachineConfig, TaskEntry, Tasks};
use crate::disk::paths::{RuntimePaths, StatePaths, rimz_home, workspaces_dir};
use crate::ids::TaskId;
use crate::trust::TrustState;
use crate::workspace::WorkspaceResolver;

pub fn project_config_path(project_root: &Path) -> PathBuf {
    config_edit::TaskStore::Project(project_root).path()
}

/// The state dir holding `project_root`'s instance tasks, minted when unborn.
fn instance_root(project_root: &Path) -> Result<PathBuf> {
    StatePaths::for_project_root(project_root)
        .map(|paths| paths.root)
        .with_context(|| format!("locating state for {}", project_root.display()))
}

pub fn workspace_instance_roots() -> BTreeSet<PathBuf> {
    let Ok(workspaces) = std::fs::read_dir(workspaces_dir()) else {
        return BTreeSet::new();
    };
    workspaces
        .flatten()
        .flat_map(|workspace| instances::load_from(&workspace.path()).0.into_values())
        .map(|entry| entry.resolved_root())
        .collect()
}

/// A checkout path an armed loop task still needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreePin {
    pub name: String,
    pub path: PathBuf,
}

/// Paths needed by an enabled machine or instance task, including while paused.
pub fn worktree_pins(
    name: &str,
    task: &LoadedTask,
    arming: Option<&Arming>,
    now: Timestamp,
) -> Vec<WorktreePin> {
    let entry = task.entry();
    if matches!(task.source(), TaskSource::Project { .. })
        || matches!(
            ArmState::resolve(arming, task.source(), now),
            ArmState::Disabled(_)
        )
        || entry.deadline.is_some_and(|deadline| now >= deadline)
    {
        return Vec::new();
    }
    let root = entry.resolved_root();
    let mut paths = vec![root.clone()];
    if !entry.each_worktree {
        let dir = entry.run_dir();
        if dir != root {
            paths.push(dir);
        }
    }
    paths
        .into_iter()
        .map(|path| WorktreePin {
            name: name.to_owned(),
            path,
        })
        .collect()
}

/// Read machine-wide task protection strictly, without loading project config.
pub fn load_worktree_pins() -> Result<Vec<WorktreePin>> {
    let machine = MachineConfig::load_loop()?;
    pins_from_sources(
        &workspaces_dir(),
        machine.tasks,
        &arming::load()?,
        Timestamp::now(),
    )
}

fn pins_from_sources(
    workspaces: &Path,
    machine: Tasks,
    arming: &BTreeMap<String, Arming>,
    now: Timestamp,
) -> Result<Vec<WorktreePin>> {
    let mut roots = BTreeMap::<PathBuf, (Tasks, Tasks)>::new();
    for workspace in std::fs::read_dir(workspaces)
        .with_context(|| format!("listing {}", workspaces.display()))?
    {
        let workspace = workspace.with_context(|| format!("listing {}", workspaces.display()))?;
        let path = workspace.path();
        // A room pruned since the listing carries no tasks to protect.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        }
        for (name, entry) in instances::load_strict_from(&path)?.0 {
            let (instances, _) = roots.entry(entry.resolved_root()).or_default();
            instances.0.insert(name, entry);
        }
    }
    for (name, entry) in machine.0 {
        let (_, machine) = roots.entry(entry.resolved_root()).or_default();
        machine.0.insert(name, entry);
    }
    let mut pins = Vec::new();
    for (root, (instances, machine)) in roots {
        let catalog = TaskCatalog::from_layers(instances, machine, None, Some(&root));
        for (name, task) in catalog.visible() {
            pins.extend(worktree_pins(name, task, arming.get(&task.key(name)), now));
        }
    }
    Ok(pins)
}

#[cfg(test)]
#[path = "catalog_pin_tests.rs"]
mod pin_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskSource {
    Config,
    Instance,
    Project { state: TrustState },
}

impl TaskSource {
    pub const fn blocked_state(self) -> Option<TrustState> {
        match self {
            Self::Project { state } if !matches!(state, TrustState::Trusted) => Some(state),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Config => "machine",
            Self::Instance => "state",
            Self::Project { .. } => "project",
        }
    }

    pub fn path(self, entry: &TaskEntry) -> PathBuf {
        match self {
            Self::Config => MachineConfig::loop_path(),
            // A display path: an unreadable state tree still names where the
            // tasks would live.
            Self::Instance => instance_root(&entry.resolved_root())
                .map_or_else(|_| workspaces_dir(), |root| instances::path(&root)),
            Self::Project { .. } => config_edit::TaskStore::Project(&entry.root).path(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedTask {
    entry: TaskEntry,
    source: TaskSource,
    shape: TaskShape,
}

impl LoadedTask {
    pub(crate) fn new(name: &str, entry: TaskEntry, source: TaskSource) -> Self {
        let shape = TaskShape::compile(name, &entry);
        Self {
            entry,
            source,
            shape,
        }
    }

    pub fn entry(&self) -> &TaskEntry {
        &self.entry
    }

    pub const fn kind(&self) -> super::TaskKind {
        self.shape.kind
    }

    /// The binding key of this task's record: what arming, strike and fire views index by.
    pub fn key(&self, name: &str) -> String {
        self.binding(name).key()
    }

    pub fn key_for_root(&self, name: &str, root: &Path) -> String {
        Binding::for_task(name, self.source(), root).key()
    }

    pub fn binding(&self, name: &str) -> Binding {
        Binding::for_task(name, self.source(), &self.entry().resolved_root())
    }

    pub fn enable(
        &self,
        name: &str,
        observed: Option<&Arming>,
        now: Timestamp,
    ) -> Result<Option<Arming>> {
        let binding = self.binding(name);
        let already = ArmState::resolve(observed, self.source(), now) == ArmState::Live
            && observed.is_none_or(|record| record.enabled && record.strikes.is_none());
        if already {
            strikes::clear(&binding)?;
            return Ok(None);
        }
        let enabled = arming::enable(&binding)?;
        strikes::clear(&binding)?;
        Ok(Some(enabled))
    }

    pub const fn source(&self) -> TaskSource {
        self.source
    }

    pub fn action(&self) -> Result<&TaskAction, &TaskActionErr> {
        self.shape.action()
    }

    pub fn trigger(&self) -> &std::result::Result<ParsedTrigger, ScheduleErr> {
        self.shape.trigger()
    }

    pub(super) const fn is_ephemeral(&self) -> bool {
        self.shape.is_ephemeral()
    }
}

#[derive(Clone, Debug)]
pub struct TaskCatalog {
    visible: BTreeMap<String, LoadedTask>,
    runnable: BTreeMap<String, LoadedTask>,
    skipped_machine: Tasks,
    /// The binding of every row the catalog read, skipped machine rows included: what gc
    /// keeps a live task record for.
    row_bindings: BTreeSet<Binding>,
    project_root: Option<PathBuf>,
}

impl TaskCatalog {
    /// Strict interactive load. Malformed machine, instance, or project state
    /// fails at command entry.
    pub fn load(project_root: Option<&Path>) -> Result<Self> {
        Self::load_scoped(project_root, false)
    }

    /// Strict room-only load, filtering machine tasks before name precedence.
    pub fn load_room(root: &Path) -> Result<Self> {
        Self::load_scoped(Some(root), true)
    }

    /// Whether a strict catalog load failed reading machine or instance tasks.
    pub fn source_unavailable(error: &anyhow::Error) -> bool {
        error.is::<crate::config::ConfigErr>()
            || matches!(
                error.downcast_ref::<instances::InstanceErr>(),
                Some(instances::InstanceErr::Read { .. } | instances::InstanceErr::Parse { .. })
            )
    }

    fn load_scoped(project_root: Option<&Path>, room_only: bool) -> Result<Self> {
        let instances = project_root
            .map(|root| anyhow::Ok(instances::load_strict_from(&instance_root(root)?)?))
            .transpose()?
            .unwrap_or_default();
        let machine = MachineConfig::load_loop().context("reading per-machine loop.toml")?;
        let mut machine_tasks = machine.tasks;
        if room_only {
            machine_tasks
                .0
                .retain(|_, entry| Some(entry.resolved_root().as_path()) == project_root);
        }
        let project = project_root
            .map(|root| crate::config::effective::project_tasks(root, &rimz_home()))
            .transpose()?
            .flatten()
            .map(|project| (project.tasks, project.state));
        Ok(
            Self::from_layers(instances, machine_tasks, project, project_root)
                .with_skipped_machine(machine.skipped_tasks),
        )
    }

    /// Best-effort load for elder, doctor, and maintenance reads.
    pub fn load_lenient(project_root: Option<&Path>) -> Self {
        let state_root = project_root.and_then(|root| instance_root(root).ok());
        Self::lenient(project_root, state_root.as_deref())
    }

    /// Best-effort load of the room at `project_root`, its instance rows read from the
    /// room's own `state` rather than resolved from the root.
    pub fn load_room_lenient(project_root: &Path, state: &StatePaths) -> Self {
        Self::lenient(Some(project_root), Some(&state.root))
    }

    fn lenient(project_root: Option<&Path>, state_root: Option<&Path>) -> Self {
        let instances = state_root.map(instances::load_from).unwrap_or_default();
        let machine = LoopConfig::load_lenient();
        let project = project_root.and_then(|root| {
            crate::config::effective::project_tasks(root, &rimz_home())
                .ok()
                .flatten()
                .map(|project| (project.tasks, project.state))
        });
        Self::from_layers(instances, machine.tasks, project, project_root)
            .with_skipped_machine(machine.skipped_tasks)
    }

    fn from_layers(
        instances: Tasks,
        machine: Tasks,
        project: Option<(Tasks, TrustState)>,
        project_root: Option<&Path>,
    ) -> Self {
        let mut row_bindings = instances
            .0
            .iter()
            .map(|(name, entry)| {
                Binding::for_task(name, TaskSource::Instance, &entry.resolved_root())
            })
            .chain(machine.0.iter().map(|(name, entry)| {
                Binding::for_task(name, TaskSource::Config, &entry.resolved_root())
            }))
            .collect::<BTreeSet<_>>();
        if let Some((tasks, state)) = &project {
            row_bindings.extend(tasks.0.iter().map(|(name, entry)| {
                Binding::for_task(
                    name,
                    TaskSource::Project { state: *state },
                    &entry.resolved_root(),
                )
            }));
        }
        let mut runnable = merge_base(instances, machine);
        let mut visible = runnable.clone();
        if let Some((project, state)) = project {
            let project = project.0.into_iter().map(|(name, entry)| {
                let task = LoadedTask::new(&name, entry, TaskSource::Project { state });
                (name, task)
            });
            if state == TrustState::Trusted {
                let project = project.collect::<Vec<_>>();
                visible.extend(project.iter().cloned());
                runnable.extend(project);
            } else {
                for (name, task) in project {
                    visible.insert(name.clone(), task.clone());
                    runnable.entry(name).or_insert(task);
                }
            }
        }
        Self {
            visible,
            runnable,
            skipped_machine: Tasks::default(),
            row_bindings,
            project_root: project_root.map(Path::to_path_buf),
        }
    }

    fn with_skipped_machine(mut self, tasks: Tasks) -> Self {
        self.row_bindings.extend(
            tasks
                .0
                .iter()
                .map(|(name, entry)| Binding::for_task(name, TaskSource::Config, &entry.root)),
        );
        self.skipped_machine = tasks;
        self
    }

    pub fn visible(&self) -> &BTreeMap<String, LoadedTask> {
        &self.visible
    }

    /// A `loop-<petname>` name no task of the catalog holds, for a `rimz loop add` without one.
    pub fn mint_name(&self) -> String {
        let taken = |name: &str| {
            self.visible.contains_key(name) || self.skipped_machine.0.contains_key(name)
        };
        let petname = crate::agents::petname::mint(|name| taken(&format!("loop-{name}")));
        format!("loop-{petname}")
    }

    pub fn for_run(&self, name: &str) -> Option<&LoadedTask> {
        self.runnable.get(name)
    }

    pub(super) fn runnable(&self) -> &BTreeMap<String, LoadedTask> {
        &self.runnable
    }

    pub fn replace_machine(&self, name: &str, entry: &TaskEntry) -> Result<TaskMutation> {
        if matches!(
            self.visible.get(name).map(LoadedTask::source),
            Some(TaskSource::Project { .. })
        ) {
            bail!(
                "loop task `{name}` is project-owned in {}; use `rimz loop add --project` or choose another name",
                config_edit::TaskStore::Project(&entry.root)
                    .path()
                    .display()
            );
        }
        let instance_root = StatePaths::for_project_root(&entry.resolved_root())?;
        if entry.wait.is_some() || super::ephemeral_lifetime(entry) {
            crate::workspace::record::read(&instance_root.workspace_record).map_err(|_| {
                crate::workspace::WorkspaceErr::NoRoom {
                    location: format!("at {}", entry.resolved_root().display()),
                }
            })?;
            instances::insert(&instance_root, name, entry)?;
            config_edit::remove_machine_at_root(name, &entry.resolved_root())?;
        } else {
            config_edit::set_entry(config_edit::TaskStore::Machine, name, entry)?;
            instances::remove(&instance_root, name, None)?;
        }
        let source = TaskSource::from_entry(entry);
        self.clear_task_state(name, &entry.resolved_root())?;
        let binding = Binding::for_task(name, source, &entry.resolved_root());
        Ok(TaskMutation {
            changed: true,
            cleared_overlays: mint(&binding, None)?,
        })
    }

    pub fn replace_project(
        &self,
        name: &str,
        project_root: &Path,
        entry: &TaskEntry,
    ) -> Result<TaskMutation> {
        config_edit::set_entry(config_edit::TaskStore::Project(project_root), name, entry)?;
        self.clear_task_state(name, project_root)?;
        let source = TaskSource::Project {
            state: crate::trust::status(project_root)?.state,
        };
        let enabled = Arming {
            enabled: true,
            at: Some(Timestamp::now()),
            pause_until: None,
            strikes: None,
        };
        Ok(TaskMutation {
            changed: true,
            cleared_overlays: mint(
                &Binding::for_task(name, source, project_root),
                Some(enabled),
            )?,
        })
    }

    pub fn remove(&self, name: &str) -> Result<TaskMutation> {
        let Some(task) = self.visible.get(name) else {
            let Some(entry) = self.skipped_machine.0.get(name) else {
                return Ok(TaskMutation::default());
            };
            return Ok(TaskMutation {
                changed: config_edit::remove(config_edit::TaskStore::Machine, name)?,
                cleared_overlays: retire(
                    &Binding::for_task(name, TaskSource::Config, &entry.root),
                    false,
                )?,
            });
        };
        let root = task.entry().resolved_root();
        let binding = task.binding(name);
        let id = super::task_record::load()?
            .live(&binding.key())
            .map(|(id, _)| id.clone());
        let changed = remove_definition(name, task)?;
        if changed && let Some(id) = &id {
            let paths = StatePaths::for_project_root(&root)?;
            cancel_unacted_runs(&paths, &root, id, "loop task removed")?;
            super::backlog_store::remove(&paths, id.as_str())?;
            crate::harness::workflow::reassign_task(&paths, id)?;
        }
        Ok(TaskMutation {
            changed,
            cleared_overlays: retire(&binding, false)?,
        })
    }

    /// Rename a task's row and rebind its record; nothing else moves, since every other record
    /// of the task is keyed by its id.
    pub fn rename(&self, name: &str, new_name: &str) -> Result<TaskMutation> {
        if self.visible.contains_key(new_name) || self.skipped_machine.0.contains_key(new_name) {
            bail!("loop task `{new_name}` already exists");
        }
        let Some(task) = self.visible.get(name) else {
            let Some(entry) = self.skipped_machine.0.get(name) else {
                return Ok(TaskMutation::default());
            };
            let changed = config_edit::rename(config_edit::TaskStore::Machine, name, new_name)?;
            let rebound = rebind(
                &Binding::for_task(name, TaskSource::Config, &entry.root),
                Binding::for_task(new_name, TaskSource::Config, &entry.root),
            )?;
            return Ok(TaskMutation {
                changed,
                cleared_overlays: rebound,
            });
        };
        let changed = match task.source() {
            TaskSource::Config => {
                config_edit::rename(config_edit::TaskStore::Machine, name, new_name)?
            }
            TaskSource::Instance => instances::rename(
                &StatePaths::for_project_root(&task.entry().resolved_root())?,
                name,
                new_name,
            )?,
            TaskSource::Project { .. } => config_edit::rename(
                config_edit::TaskStore::Project(&task.entry().root),
                name,
                new_name,
            )?,
        };
        Ok(TaskMutation {
            changed,
            cleared_overlays: rebind(&task.binding(name), task.binding(new_name))?,
        })
    }

    /// Remove only the definition selected by catalog precedence, and retire its record, a
    /// delivery's too, so `loop logs <name>` still finds its history. Arming and strikes stay
    /// with the retired record because this is scheduled consumption, not an interactive edit.
    pub(super) fn consume_scheduled(&self, name: &str) -> Result<TaskMutation> {
        let Some(task) = self.runnable.get(name) else {
            return Ok(TaskMutation::default());
        };
        let changed = remove_definition(name, task)?;
        retire(&task.binding(name), false)?;
        Ok(TaskMutation {
            changed,
            cleared_overlays: false,
        })
    }

    /// Before a task is replaced: cancel its live runs that have not acted and drop its
    /// waiting triggers, at its new root and at the root it had.
    fn clear_task_state(&self, name: &str, root: &Path) -> Result<()> {
        let Some(task) = self.visible.get(name) else {
            return Ok(());
        };
        let records = super::task_record::load()?;
        let Some((id, _)) = records.live(&task.key(name)) else {
            return Ok(());
        };
        let old_root = task.entry().resolved_root();
        let roots = if old_root == root {
            vec![old_root]
        } else {
            vec![root.to_owned(), old_root]
        };
        for root in roots {
            let paths = StatePaths::for_project_root(&root)?;
            cancel_unacted_runs(&paths, &root, id, "loop task redefined")?;
            super::backlog_store::remove(&paths, id.as_str())?;
        }
        Ok(())
    }

    /// Prune the task record: retire a live record of the catalog's scopes whose row is gone,
    /// drop a settled mark whose checkout is gone, and delete a retired record once every fire it
    /// marked is recorded, each only once nothing shows it (`Shown`). Also removes the files
    /// the task record replaced.
    pub fn prune_records(&self) -> Result<usize> {
        let scopes = Scope::known(self.project_root.as_deref());
        let shown = Shown::read(&super::task_record::load()?);
        let now = Timestamp::now();
        let pruned = task_record_store::mutate(|records| {
            let before = records.clone();
            for (id, record) in &mut records.tasks {
                if record.is_live()
                    && scopes.contains(&record.binding.scope)
                    && !self.row_bindings.contains(&record.binding)
                {
                    record.retired_at = Some(now);
                }
                let launched = shown.launched.contains(id);
                record.marks.retain(|checkout, mark| {
                    mark.result.is_none() || checkout.is_dir() || launched
                });
            }
            records.tasks.retain(|id, record| {
                record.is_live()
                    || record.marks.values().any(|mark| mark.result.is_none())
                    || shown.launched.contains(id)
                    || shown.logged.contains(id)
            });
            before
                .tasks
                .iter()
                .filter(|(id, record)| records.tasks.get(id) != Some(record))
                .count()
        })?;
        let room = self
            .project_root
            .as_deref()
            .map(StatePaths::for_project_root)
            .transpose()?;
        Ok(pruned + sweep_retired_files(room.as_ref()))
    }

    pub fn reap_dead_deliveries() -> Result<usize> {
        let catalog = Self::load_lenient(None);
        let mut reaped = catalog.reap_catalog_deliveries()?;
        for root in workspace_instance_roots() {
            reaped += Self::reap_dead_deliveries_for(&root)?;
        }
        Ok(reaped)
    }

    /// Reap this room's instance rows without loading machine or project tasks.
    pub fn reap_dead_deliveries_for(project_root: &Path) -> Result<usize> {
        Self::from_layers(
            instances::load_from(&instance_root(project_root)?),
            Tasks::default(),
            None,
            Some(project_root),
        )
        .reap_catalog_deliveries()
    }

    fn reap_catalog_deliveries(&self) -> Result<usize> {
        let mut reaped = 0;
        for (name, task) in self.runnable.clone() {
            let target = match task.action() {
                Ok(TaskAction::Deliver(target)) => target.clone(),
                Ok(TaskAction::Shell(_) | TaskAction::Launch(_))
                | Err(TaskActionErr::Refused { .. }) => {
                    continue;
                }
                // RimZ is the only writer of instance rows, so one that no
                // longer compiles can never fire or retire: reap it. User-owned
                // rows keep rendering `<invalid>` for the user to fix.
                Err(err) if task.source() == TaskSource::Instance => {
                    tracing::debug!(task = %name, error = %err, "undeliverable loop instance reaped by schedule gc");
                    self.reap_scheduled(&name, &task)?;
                    reaped += 1;
                    continue;
                }
                Err(err) => {
                    tracing::debug!(task = %name, error = %err, "invalid loop task skipped by schedule gc");
                    continue;
                }
            };
            match delivery_holder(&task.entry().resolved_root(), &target) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.reap_scheduled(&name, &task)?;
                    reaped += 1;
                }
                Err(err) => {
                    tracing::debug!(task = %name, error = %err, "loop schedule gc skipped task");
                }
            }
        }
        Ok(reaped)
    }

    fn reap_scheduled(&self, name: &str, task: &LoadedTask) -> Result<()> {
        if task
            .trigger()
            .as_ref()
            .is_ok_and(|parsed| match parsed.trigger {
                super::Trigger::Watch(_) => true,
                super::Trigger::Schedule(_)
                | super::Trigger::Signal { .. }
                | super::Trigger::Condition { .. } => false,
            })
            && let Some(runtime) =
                WorkspaceResolver::persisted_project_root(task.entry().resolved_root())
                    .ok()
                    .and_then(|root| RuntimePaths::for_project_root(&root).ok())
        {
            let _ = super::signal::stop_watcher(&runtime, name);
        }
        self.consume_scheduled(name)?;
        Ok(())
    }
}

/// Cancel the task's live runs that have not reached an agent, each in a detached
/// `rimz workflow cancel`, which reports the run `canceled`; runs that launched one keep going.
fn cancel_unacted_runs(
    paths: &StatePaths,
    root: &Path,
    task: &crate::ids::TaskId,
    reason: &str,
) -> Result<()> {
    for run in crate::harness::workflow::unacted_task_runs(paths, task)? {
        let mut args: Vec<std::ffi::OsString> = vec!["--root".into(), root.into()];
        args.extend(crate::harness::workflow::cancel_args(&run, reason));
        let spawned = RuntimePaths::for_state(paths)
            .map_err(anyhow::Error::from)
            .and_then(|runtime| {
                crate::child_process::spawn_detached_rimz(&runtime, &args, "loop-redefine")
                    .map_err(anyhow::Error::from)
            });
        if let Err(error) = spawned {
            tracing::warn!(
                %task,
                %run,
                error = %format!("{error:#}"),
                "cannot cancel a run of a changed loop task"
            );
        }
    }
    Ok(())
}

/// The files the task record replaced: the machine's arming, strike and fire overlays, and a
/// room's workflow launch memory and capacity waits, each with its lock.
const RETIRED_MACHINE_FILES: [&str; 4] = [
    "loop-arming",
    "loop-strikes",
    "loop-strike-runs",
    "loop-fires",
];
const RETIRED_ROOM_FILES: [&str; 3] = [
    "loop-workflow-launches",
    "loop-capacity-waits",
    "loop-launches",
];

/// What still shows a task's record, so `rimz gc` keeps it once its fires are settled.
struct Shown {
    /// A launch carrying one of the task's fire ids is alive, or a worktree marker carries one,
    /// in a room its marks name; or such a room does not read.
    launched: BTreeSet<TaskId>,
    /// A row of the machine's loop run history, current or rotated, names the task.
    logged: BTreeSet<TaskId>,
}

impl Shown {
    fn read(records: &super::task_record::TaskRecords) -> Self {
        let mut rooms = BTreeMap::<PathBuf, Option<ShownRoom>>::new();
        let mut launched = BTreeSet::new();
        for (id, record) in &records.tasks {
            for checkout in record.marks.keys() {
                let Ok(root) = WorkspaceResolver::persisted_project_root(checkout) else {
                    launched.insert(id.clone());
                    break;
                };
                let room = rooms
                    .entry(root)
                    .or_insert_with_key(|root| ShownRoom::read(root));
                if room.as_ref().is_none_or(|room| room.shows(id)) {
                    launched.insert(id.clone());
                    break;
                }
            }
        }
        Self {
            launched,
            logged: super::run_log::logged_task_ids(),
        }
    }
}

/// One room's state as [`Shown`] reads it.
struct ShownRoom {
    paths: StatePaths,
    agents: Vec<crate::agents::AgentState>,
    /// The tasks whose fire ids the room's worktree markers carry.
    marked: BTreeSet<TaskId>,
}

impl ShownRoom {
    /// The room at `root`; `None` when its state or its worktrees do not read.
    fn read(root: &Path) -> Option<Self> {
        let read = || -> Result<Self> {
            let paths = StatePaths::for_project_root(root)?;
            let agents = super::launches::room_agents(&paths)?;
            let owned = if root.is_dir() {
                match crate::worktree::discover_owned(root) {
                    Err(crate::worktree::WorktreeErr::NotRepo) => Vec::new(),
                    owned => owned?,
                }
            } else {
                Vec::new()
            };
            let marked = owned
                .into_iter()
                .filter_map(|tree| tree.marker.loop_fire.map(|fire| fire.task()))
                .collect();
            Ok(Self {
                paths,
                agents,
                marked,
            })
        };
        read()
            .inspect_err(|error| {
                tracing::debug!(
                    root = %root.display(),
                    error = %format!("{error:#}"),
                    "room unreadable; its loop task records kept"
                );
            })
            .ok()
    }

    /// Whether the room shows task `id`: a worktree marker carries one of its fire ids, or a
    /// launch carrying one is alive. A read that fails shows it.
    fn shows(&self, id: &TaskId) -> bool {
        self.marked.contains(id)
            || super::launches::alive(id, &self.paths, &self.agents)
                .map_or(true, |alive| !alive.is_empty())
    }
}

fn sweep_retired_files(room: Option<&StatePaths>) -> usize {
    let loops = crate::disk::paths::loops_dir();
    let machine = RETIRED_MACHINE_FILES.iter().flat_map(|stem| {
        [
            loops.join(format!("{stem}.json")),
            loops.join(format!("{stem}.lock")),
        ]
    });
    let rooms = room.into_iter().flat_map(|paths| {
        RETIRED_ROOM_FILES.iter().flat_map(move |stem| {
            [
                StatePaths::class_path(
                    &paths.root,
                    crate::disk::paths::Class::Records,
                    format!("{stem}.json"),
                ),
                paths.lock_path(&format!("{stem}.lock")),
            ]
        })
    });
    machine
        .chain(rooms)
        .filter(|path| std::fs::remove_file(path).is_ok())
        .count()
}

impl TaskSource {
    fn from_entry(entry: &TaskEntry) -> Self {
        if entry.wait.is_some() || super::ephemeral_lifetime(entry) {
            Self::Instance
        } else {
            Self::Config
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskMutation {
    changed: bool,
    cleared_overlays: bool,
}

impl TaskMutation {
    pub const fn changed(&self) -> bool {
        self.changed
    }

    pub const fn cleared_overlays(&self) -> bool {
        self.cleared_overlays
    }
}

fn merge_base(instances: Tasks, machine: Tasks) -> BTreeMap<String, LoadedTask> {
    let mut tasks = instances
        .0
        .into_iter()
        .map(|(name, entry)| {
            let task = LoadedTask::new(&name, entry, TaskSource::Instance);
            (name, task)
        })
        .collect::<BTreeMap<_, _>>();
    for (name, entry) in machine.0 {
        if tasks
            .get(&name)
            .is_some_and(|task| task.entry().resolved_root() != entry.resolved_root())
        {
            continue;
        }
        let task = LoadedTask::new(&name, entry, TaskSource::Config);
        tasks.insert(name, task);
    }
    tasks
}

/// Mint a record for a task added over `binding`, retiring the one it replaces; whether that
/// one held arming or strikes.
fn mint(
    binding: &Binding,
    arming: Option<Arming>,
) -> std::result::Result<bool, task_record_store::TaskStoreErr> {
    task_record_store::mutate(|records| {
        let cleared = records
            .live(&binding.key())
            .is_some_and(|(_, record)| record.arming.is_some() || record.strikes > 0);
        let id = records.mint(binding, Timestamp::now());
        if let Some(record) = records.get_mut(&id) {
            record.arming = arming;
        }
        cleared
    })
}

/// Retire the record bound to `binding`, or delete it (a live record no row holds, under a
/// name a new delivery takes); whether it held arming or strikes.
pub(super) fn retire(
    binding: &Binding,
    delete: bool,
) -> std::result::Result<bool, task_record_store::TaskStoreErr> {
    let key = binding.key();
    let holds = |records: &super::task_record::TaskRecords| {
        records
            .live(&key)
            .is_some_and(|(_, record)| record.arming.is_some() || record.strikes > 0)
    };
    if super::task_record::load()?.live(&key).is_none() {
        return Ok(false);
    }
    task_record_store::mutate(|records| {
        let held = holds(records);
        if delete {
            records.delete(&key);
        } else {
            records.retire(&key, Timestamp::now());
        }
        held
    })
}

/// Move the record bound to `old` onto `new`; whether there was one.
fn rebind(
    old: &Binding,
    new: Binding,
) -> std::result::Result<bool, task_record_store::TaskStoreErr> {
    let key = old.key();
    if super::task_record::load()?.live(&key).is_none() {
        return Ok(false);
    }
    task_record_store::mutate(|records| records.rebind(&key, new, Timestamp::now()))
}

fn remove_definition(name: &str, task: &LoadedTask) -> Result<bool> {
    match task.source() {
        TaskSource::Config => Ok(config_edit::remove(config_edit::TaskStore::Machine, name)?),
        TaskSource::Instance => Ok(instances::remove(
            &StatePaths::for_project_root(&task.entry().resolved_root())?,
            name,
            Some(task.entry()),
        )?),
        TaskSource::Project { .. } => Ok(config_edit::remove(
            config_edit::TaskStore::Project(&task.entry().root),
            name,
        )?),
    }
}

/// Where a wake for `target`, armed in the room at `root`, goes now: the
/// session its agent's holder runs as, read on Audit rows so a session a
/// restart or molt replaced still resolves, or `None` once the agent has ended
/// or its room is gone. A provisional session is a holder still starting,
/// which a wake can only queue for under the handle the holder's row carries.
pub(super) fn delivery_holder(
    root: &Path,
    target: &crate::config::TaskTarget,
) -> Result<Option<crate::config::TaskTarget>> {
    let project_root = WorkspaceResolver::persisted_project_root(root)
        .with_context(|| format!("resolving persisted project root at {}", root.display()))?;
    let paths = StatePaths::for_project_root(&project_root)?;
    if crate::workspace::record::read(&paths.workspace_record).is_err() {
        return Ok(None);
    }
    let runtime = RuntimePaths::for_state(&paths)?;
    let Some(store) = Store::open_existing(paths, runtime) else {
        return Ok(None);
    };
    let rows = store.audit_rows().context("reading agent rows")?;
    Ok(super::pending::target_holder(rows.iter(), target)
        .filter(|holder| !holder.session_over())
        .map(|holder| crate::config::TaskTarget {
            kind: holder.kind.clone(),
            session: holder.agent_id.clone(),
            handle: holder
                .name
                .as_ref()
                .filter(|_| holder.agent_id.is_provisional())
                .map_or_else(
                    || target.handle.clone(),
                    |name| match crate::address::lane_label(holder) {
                        Some(lane) => format!("@{name}#{lane}"),
                        None => format!("@{name}"),
                    },
                ),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_reaping_preserves_other_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let paths_a = StatePaths::for_project_root(&root_a).unwrap();
        let paths_b = StatePaths::for_project_root(&root_b).unwrap();
        for (root, paths) in [(&root_a, &paths_a), (&root_b, &paths_b)] {
            let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).unwrap();
            crate::workspace::record::write(
                paths,
                &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
            )
            .unwrap();
            instances::insert(
                paths,
                "actionless",
                &TaskEntry {
                    root: root.clone(),
                    signal: Some("ci.failed".into()),
                    ..TaskEntry::default()
                },
            )
            .unwrap();
        }
        let before_b = std::fs::read(instances::path(&paths_b.root)).unwrap();
        assert_eq!(TaskCatalog::reap_dead_deliveries_for(&root_a).unwrap(), 1);
        assert!(instances::load_from(&paths_a.root).0.is_empty());
        assert_eq!(
            std::fs::read(instances::path(&paths_b.root)).unwrap(),
            before_b
        );
        assert_eq!(TaskCatalog::reap_dead_deliveries_for(&root_a).unwrap(), 0);
    }

    fn task(label: &str) -> TaskEntry {
        TaskEntry {
            args: vec!["true".to_owned()],
            timeout: Some("1m".to_owned()),
            label: Some(label.to_owned()),
            root: PathBuf::from("/repo"),
            every: Some("1h".to_owned()),
            ..TaskEntry::default()
        }
    }

    #[test]
    fn pending_deliveries_filter_scope_and_sort_one_shot_triggers() {
        use super::super::pending::{pending_waits_by_session, session_deliveries};
        use crate::config::TaskTarget;
        use crate::ids::{AgentKind, AgentSessionId};

        let target = TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: AgentSessionId::from("session"),
            handle: "coder".into(),
        };
        let command = TaskEntry {
            root: PathBuf::from("/repo"),
            wait: Some(target.clone()),
            watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
            ..TaskEntry::default()
        };
        let signal = TaskEntry {
            watch: None,
            signal: Some("pr.merged".into()),
            ..command.clone()
        };
        let timer = TaskEntry {
            watch: None,
            fire_at: Some("2026-06-01T12:00:00Z".parse().unwrap()),
            wait_meta: Some(crate::config::WaitMeta {
                armed_at: "2026-06-01T10:00:00Z".parse().unwrap(),
                delay: None,
                reader: None,
            }),
            ..command.clone()
        };
        let catalog = TaskCatalog::from_layers(
            Tasks(BTreeMap::from([
                ("z-command".into(), command.clone()),
                ("a-command".into(), command.clone()),
                (
                    "m-pid".into(),
                    TaskEntry {
                        watch: Some(crate::config::WatchSpec::Pid { pid: 16776 }),
                        ..command.clone()
                    },
                ),
                ("a-later-timer".into(), timer.clone()),
                (
                    "z-sooner-timer".into(),
                    TaskEntry {
                        fire_at: Some("2026-06-01T11:00:00Z".parse().unwrap()),
                        ..timer
                    },
                ),
                ("signal".into(), signal.clone()),
                (
                    "standing".into(),
                    TaskEntry {
                        repeat: true,
                        ..signal
                    },
                ),
                (
                    "other-root".into(),
                    TaskEntry {
                        root: PathBuf::from("/elsewhere"),
                        ..command.clone()
                    },
                ),
                (
                    "other-session".into(),
                    TaskEntry {
                        wait: Some(TaskTarget {
                            session: AgentSessionId::from("other"),
                            ..target.clone()
                        }),
                        ..command.clone()
                    },
                ),
                (
                    "no-delivery".into(),
                    TaskEntry {
                        wait: None,
                        args: vec!["true".to_owned()],
                        timeout: Some("1m".to_owned()),
                        ..command.clone()
                    },
                ),
            ])),
            Tasks(BTreeMap::from([("machine".into(), command)])),
            None,
            Some(Path::new("/repo")),
        );
        let agent = crate::testkit::agent_state(
            target.kind.as_str(),
            target.session.as_str(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        let session = (target.kind, target.session);
        assert_eq!(
            session_deliveries(&catalog, Path::new("/repo"), Some(&agent), &[])
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            vec![
                "a-command",
                "a-later-timer",
                "m-pid",
                "signal",
                "standing",
                "z-command",
                "z-sooner-timer"
            ]
        );
        let waits = pending_waits_by_session(&catalog, Path::new("/repo"));
        assert_eq!(
            waits[&session]
                .iter()
                .map(|wait| wait.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "z-sooner-timer",
                "a-later-timer",
                "a-command",
                "m-pid",
                "z-command",
                "signal"
            ]
        );
        assert_eq!(waits.len(), 2);
    }

    #[test]
    fn foreign_machine_preserves_instance_and_both_bindings() {
        let instance = task("instance");
        let mut machine = task("machine");
        machine.root = PathBuf::from("/other");
        let catalog = TaskCatalog::from_layers(
            Tasks(BTreeMap::from([("same".to_owned(), instance.clone())])),
            Tasks(BTreeMap::from([("same".to_owned(), machine.clone())])),
            None,
            Some(Path::new("/repo")),
        );

        for loaded in [&catalog.visible()["same"], catalog.for_run("same").unwrap()] {
            assert_eq!(loaded.source(), TaskSource::Instance);
            assert_eq!(loaded.entry(), &instance);
        }
        for (source, entry) in [
            (TaskSource::Instance, instance),
            (TaskSource::Config, machine),
        ] {
            assert!(catalog.row_bindings.contains(&Binding::for_task(
                "same",
                source,
                &entry.resolved_root(),
            )));
        }
    }

    #[test]
    fn skipped_machine_rows_keep_their_bindings_for_gc() {
        let skipped = Tasks(BTreeMap::from([
            ("bare".to_owned(), TaskEntry::default()),
            (
                "relative".to_owned(),
                TaskEntry {
                    root: PathBuf::from("nowhere/x"),
                    ..TaskEntry::default()
                },
            ),
        ]));
        let catalog = TaskCatalog::from_layers(
            Tasks::default(),
            Tasks::default(),
            None,
            Some(Path::new("/repo")),
        )
        .with_skipped_machine(skipped);

        for name in ["bare", "relative"] {
            assert!(
                catalog.row_bindings.contains(&Binding::for_task(
                    name,
                    TaskSource::Config,
                    Path::new("/repo"),
                )),
                "{name}: gc would prune the record it keeps once its root is fixed"
            );
        }
    }

    #[test]
    fn visible_and_runnable_precedence_diverge_for_untrusted_project() {
        let catalog = TaskCatalog::from_layers(
            Tasks(BTreeMap::from([("same".to_owned(), task("instance"))])),
            Tasks(BTreeMap::from([("same".to_owned(), task("machine"))])),
            Some((
                Tasks(BTreeMap::from([
                    ("same".to_owned(), task("project")),
                    ("blocked".to_owned(), task("blocked")),
                ])),
                TrustState::Untrusted,
            )),
            Some(Path::new("/repo")),
        );

        assert_eq!(
            catalog.visible()["same"].entry.label.as_deref(),
            Some("project")
        );
        assert_eq!(
            catalog.for_run("same").unwrap().entry.label.as_deref(),
            Some("machine")
        );
        assert!(matches!(
            catalog.for_run("blocked").unwrap().source,
            TaskSource::Project {
                state: TrustState::Untrusted
            }
        ));
    }

    #[test]
    fn trusted_project_wins_both_merges() {
        let catalog = TaskCatalog::from_layers(
            Tasks(BTreeMap::from([("same".to_owned(), task("instance"))])),
            Tasks(BTreeMap::from([("same".to_owned(), task("machine"))])),
            Some((
                Tasks(BTreeMap::from([("same".to_owned(), task("project"))])),
                TrustState::Trusted,
            )),
            Some(Path::new("/repo")),
        );

        assert_eq!(
            catalog.visible()["same"].entry.label.as_deref(),
            Some("project")
        );
        assert_eq!(
            catalog.for_run("same").unwrap().entry.label.as_deref(),
            Some("project")
        );
        assert!(
            catalog
                .row_bindings
                .iter()
                .any(|binding| binding.key() == "machine::same")
        );
        assert!(catalog.row_bindings.contains(&Binding::for_task(
            "same",
            TaskSource::Project {
                state: TrustState::Trusted,
            },
            Path::new("/repo"),
        )));
    }

    #[test]
    fn empty_project_layer_keeps_its_project_root() {
        let catalog = TaskCatalog::from_layers(
            Tasks::default(),
            Tasks::default(),
            Some((Tasks::default(), TrustState::Trusted)),
            Some(Path::new("/repo")),
        );

        assert_eq!(catalog.project_root.as_deref(), Some(Path::new("/repo")));
    }

    #[test]
    fn malformed_schedule_stays_visible_with_valid_action() {
        let mut entry = task("still runnable manually");
        entry.cron = Some("0 7 * * *".to_owned());
        let catalog = TaskCatalog::from_layers(
            Tasks::default(),
            Tasks(BTreeMap::from([("broken".to_owned(), entry)])),
            None,
            None,
        );
        let loaded = &catalog.visible()["broken"];

        assert!(matches!(loaded.action(), Ok(TaskAction::Shell(_))));
        assert!(matches!(
            loaded.trigger(),
            Err(ScheduleErr::TimeConflict { .. })
        ));
        assert!(catalog.for_run("broken").is_some());
    }

    #[test]
    fn ephemeral_tasks_are_one_shot_or_deadline_bound() {
        let mut entry = task("wait");
        assert!(!super::super::TaskShape::compile("task", &entry).is_ephemeral());

        entry.every = None;
        assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());

        entry.every = Some("15m".to_owned());
        entry.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
        assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());

        entry.every = None;
        entry.signal = Some("ci.failed".to_owned());
        entry.wait_meta = Some(crate::config::WaitMeta {
            armed_at: jiff::Timestamp::UNIX_EPOCH,
            delay: None,
            reader: None,
        });
        assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());
        assert_eq!(TaskSource::from_entry(&entry), TaskSource::Instance);

        entry.signal = None;
        entry.deadline = None;
        entry.when = Some(vec!["team.stage=Done".to_owned()]);
        assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());
        entry.repeat = true;
        assert!(!super::super::TaskShape::compile("task", &entry).is_ephemeral());
    }

    #[test]
    fn when_and_signal_rows_fire_once_unless_repeat_and_clocks_always_repeat() {
        let base = TaskEntry {
            every: None,
            ..task("row")
        };
        let rows = [
            (
                "every",
                TaskEntry {
                    every: Some("1h".into()),
                    ..base.clone()
                },
                false,
            ),
            (
                "cron",
                TaskEntry {
                    cron: Some("0 * * * *".into()),
                    ..base.clone()
                },
                false,
            ),
            (
                "signal",
                TaskEntry {
                    signal: Some("ci.failed".into()),
                    ..base.clone()
                },
                true,
            ),
            (
                "when",
                TaskEntry {
                    when: Some(vec!["pr=open".into()]),
                    ..base.clone()
                },
                true,
            ),
        ];
        for (trigger, entry, spent) in rows {
            let once = super::super::TaskShape::compile("row", &entry).is_ephemeral();
            assert_eq!(once, spent, "{trigger} without repeat");
            let repeating = TaskEntry {
                repeat: true,
                ..entry
            };
            assert!(
                !super::super::TaskShape::compile("row", &repeating).is_ephemeral(),
                "{trigger} with repeat"
            );
        }
    }

    #[test]
    fn a_team_binding_is_standing_without_repeat() {
        let entry: TaskEntry = serde_json::from_value(serde_json::json!({
            "wait": {"kind": "claude", "session": "s", "handle": "@coder"},
            "team": "forge#feature",
            "signal": "ci.failed",
            "match": {"path": "/r/feature"},
            "root": "/r",
            "dir": "/r/feature",
        }))
        .unwrap();
        let shape = super::super::TaskShape::compile("binding", &entry);
        assert!(!shape.is_ephemeral());
        assert!(!shape.trigger().as_ref().unwrap().once);
    }

    #[test]
    fn vanished_delivery_root_is_not_alive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = TaskEntry {
            root: dir.path().join("vanished"),
            ..TaskEntry::default()
        };
        let target = crate::config::TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "missing-session".into(),
            handle: "@claude".to_owned(),
        };

        assert!(
            delivery_holder(&entry.resolved_root(), &target)
                .expect("check vanished target")
                .is_none()
        );
    }
}
