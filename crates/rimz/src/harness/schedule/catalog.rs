//! Loop task catalog, compiled runtime shapes, source precedence, and coordinated mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use super::{
    ParsedTrigger, ScheduleErr, TaskAction, TaskActionErr, TaskShape,
    arming::{self, ArmState, Arming, TaskKey},
    config_edit, instances, strikes,
};
use crate::Store;
use crate::config::{MachineConfig, TaskEntry, Tasks};
use crate::disk::paths::{RuntimePaths, StatePaths, rimz_home, workspaces_dir};
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
    let needs_run_dir = !entry.each_worktree
        && (entry.worktree.is_none()
            || entry.check.is_some()
            || !entry.judge.is_empty()
            || entry.when.is_some()
            || entry.watch.is_some());
    if needs_run_dir {
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
        &arming::load(),
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
    pub(super) fn new(name: &str, entry: TaskEntry, source: TaskSource) -> Self {
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

    pub fn key(&self, name: &str) -> String {
        self.key_for_root(name, &self.entry().resolved_root())
    }

    pub fn key_for_root(&self, name: &str, root: &Path) -> String {
        TaskKey::for_task(name, self.source(), root)
    }

    pub fn enable(
        &self,
        name: &str,
        observed: Option<&Arming>,
        now: Timestamp,
    ) -> Result<Option<Arming>> {
        let key = self.key(name);
        let already = ArmState::resolve(observed, self.source(), now) == ArmState::Live
            && observed.is_none_or(|record| record.enabled && record.strikes.is_none());
        if already {
            strikes::clear(&key)?;
            return Ok(None);
        }
        let enabled = arming::enable(&key)?;
        strikes::clear(&key)?;
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
    overlay_keys: BTreeSet<String>,
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
        Ok(Self::from_layers(
            instances,
            machine_tasks,
            project,
            project_root,
        ))
    }

    /// Best-effort load for elder, doctor, and maintenance reads.
    pub fn load_lenient(project_root: Option<&Path>) -> Self {
        let instances = project_root
            .and_then(|root| instance_root(root).ok())
            .map(|root| instances::load_from(&root))
            .unwrap_or_default();
        let machine = MachineConfig::load_lenient().r#loop.clone();
        let project = project_root.and_then(|root| {
            crate::config::effective::project_tasks(root, &rimz_home())
                .ok()
                .flatten()
                .map(|project| (project.tasks, project.state))
        });
        Self::from_layers(instances, machine.tasks, project, project_root)
    }

    fn from_layers(
        instances: Tasks,
        machine: Tasks,
        project: Option<(Tasks, TrustState)>,
        project_root: Option<&Path>,
    ) -> Self {
        let mut overlay_keys = instances
            .0
            .iter()
            .map(|(name, entry)| {
                TaskKey::for_task(name, TaskSource::Instance, &entry.resolved_root())
            })
            .chain(machine.0.iter().map(|(name, entry)| {
                TaskKey::for_task(name, TaskSource::Config, &entry.resolved_root())
            }))
            .collect::<BTreeSet<_>>();
        if let Some((tasks, state)) = &project {
            overlay_keys.extend(tasks.0.iter().map(|(name, entry)| {
                TaskKey::for_task(
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
            overlay_keys,
            project_root: project_root.map(Path::to_path_buf),
        }
    }

    pub fn visible(&self) -> &BTreeMap<String, LoadedTask> {
        &self.visible
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
        self.clear_declines(name, &entry.resolved_root())?;
        let key = TaskKey::for_task(name, source, &entry.resolved_root());
        Ok(TaskMutation {
            changed: true,
            cleared_overlays: clear_overlays(&key)?,
        })
    }

    pub fn replace_project(
        &self,
        name: &str,
        project_root: &Path,
        entry: &TaskEntry,
    ) -> Result<TaskMutation> {
        config_edit::set_entry(config_edit::TaskStore::Project(project_root), name, entry)?;
        self.clear_declines(name, project_root)?;
        let source = TaskSource::Project {
            state: crate::trust::status(project_root)?.state,
        };
        enable_project_overlays(name, source, project_root)
    }

    pub fn remove(&self, name: &str) -> Result<TaskMutation> {
        let Some(task) = self.visible.get(name) else {
            return Ok(TaskMutation::default());
        };
        let root = task.entry().resolved_root();
        if root.is_absolute() && task.entry().workflow.is_some() {
            super::launch_ledger::load_workflows(&StatePaths::for_project_root(&root)?)?;
        }
        let changed = remove_definition(name, task)?;
        if changed && root.is_absolute() && task.entry().stay {
            super::launch_ledger_store::remove(&StatePaths::for_project_root(&root)?, name)?;
        }
        if changed && root.is_absolute() {
            let paths = StatePaths::for_project_root(&root)?;
            match super::launch_ledger_store::remove_workflows(&paths, name) {
                Err(super::launch_ledger_store::LedgerWriteErr::Read(error))
                    if task.entry().workflow.is_none() =>
                {
                    tracing::warn!(
                        task = name,
                        root = %root.display(),
                        %error,
                        "cannot clear workflow launch memory for a non-workflow task"
                    );
                }
                result => result?,
            }
            super::backlog_store::remove(&paths, name)?;
            super::launch_ledger_store::remove_declines(&paths, name)?;
            crate::harness::workflow::reassign_task(&paths, name, None)?;
        }
        let key = task.key(name);
        let cleared_overlays = clear_overlays(&key)?;
        Ok(TaskMutation {
            changed,
            cleared_overlays,
        })
    }

    pub fn rename(&self, name: &str, new_name: &str) -> Result<TaskMutation> {
        if self.visible.contains_key(new_name) {
            bail!("loop task `{new_name}` already exists");
        }
        let Some(task) = self.visible.get(name) else {
            return Ok(TaskMutation::default());
        };
        let root = task.entry().resolved_root();
        if root.is_absolute() && task.entry().workflow.is_some() {
            super::launch_ledger::load_workflows(&StatePaths::for_project_root(&root)?)?;
        }
        if root.is_absolute()
            && task.entry().stay
            && super::launch_ledger::load(&StatePaths::for_project_root(&root)?)?
                .get(name)
                .is_some_and(|launches| !launches.is_empty())
        {
            bail!(
                "cannot rename loop task `{name}`: its resident leaders record the task name; edit it with `rimz loop add {name}`, or remove and re-add under the new name (which relaunches its checkouts)"
            );
        }
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
        let old_key = task.key(name);
        if changed && root.is_absolute() {
            if task.entry().workflow.is_some() {
                super::launch_ledger_store::rename_workflows(
                    &StatePaths::for_project_root(&root)?,
                    name,
                    new_name,
                )?;
            }
            super::backlog_store::rename(&StatePaths::for_project_root(&root)?, name, new_name)?;
            super::launch_ledger_store::rename_declines(
                &StatePaths::for_project_root(&root)?,
                name,
                new_name,
            )?;
            crate::harness::workflow::reassign_task(
                &StatePaths::for_project_root(&root)?,
                name,
                Some(new_name),
            )?;
        }
        let new_key = task.key(new_name);
        let cleared_arming = arming::rename(&old_key, &new_key)?;
        let cleared_strikes = strikes::rename(&old_key, &new_key)?;
        Ok(TaskMutation {
            changed,
            cleared_overlays: cleared_arming || cleared_strikes,
        })
    }

    /// Remove only the definition selected by catalog precedence. Arming and
    /// strike overlays stay untouched because this is scheduled consumption,
    /// not an interactive edit.
    pub(super) fn consume_scheduled(&self, name: &str) -> Result<TaskMutation> {
        let Some(task) = self.runnable.get(name) else {
            return Ok(TaskMutation::default());
        };
        Ok(TaskMutation {
            changed: remove_definition(name, task)?,
            cleared_overlays: false,
        })
    }

    fn clear_declines(&self, name: &str, root: &Path) -> Result<()> {
        super::backlog_store::remove(&StatePaths::for_project_root(root)?, name)?;
        super::launch_ledger_store::remove_declines(&StatePaths::for_project_root(root)?, name)?;
        if let Some(task) = self.visible.get(name)
            && task.entry().resolved_root() != root
        {
            super::backlog_store::remove(
                &StatePaths::for_project_root(&task.entry().resolved_root())?,
                name,
            )?;
            super::launch_ledger_store::remove_declines(
                &StatePaths::for_project_root(&task.entry().resolved_root())?,
                name,
            )?;
        }
        Ok(())
    }

    pub fn prune_orphan_overlays(&self) -> Result<usize> {
        let scopes = TaskKey::known_scopes(self.project_root.as_deref());
        Ok(arming::prune_orphans(&self.overlay_keys, &scopes)?
            + strikes::prune_orphans(&self.overlay_keys, &scopes)?)
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
                Ok(TaskAction::Spawn(_) | TaskAction::Workflow(_) | TaskAction::CheckOnly) => {
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
            match delivery_target_alive(task.entry(), &target) {
                Ok(true) => {}
                Ok(false) => {
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

fn clear_overlays(key: &str) -> Result<bool> {
    let cleared_arming = arming::remove(key)?;
    let cleared_strikes = strikes::clear(key)?;
    Ok(cleared_arming || cleared_strikes)
}

fn enable_project_overlays(
    name: &str,
    source: TaskSource,
    project_root: &Path,
) -> Result<TaskMutation> {
    let key = TaskKey::for_task(name, source, project_root);
    let cleared_arming = arming::load().contains_key(&key);
    arming::enable(&key)?;
    let cleared_strikes = strikes::clear(&key)?;
    Ok(TaskMutation {
        changed: true,
        cleared_overlays: cleared_arming || cleared_strikes,
    })
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

pub(super) fn delivery_target_alive(
    entry: &TaskEntry,
    target: &crate::config::TaskTarget,
) -> Result<bool> {
    let root = entry.resolved_root();
    let project_root = WorkspaceResolver::persisted_project_root(&root)
        .with_context(|| format!("resolving persisted project root at {}", root.display()))?;
    let paths = StatePaths::for_project_root(&project_root)?;
    if crate::workspace::record::read(&paths.workspace_record).is_err() {
        return Ok(false);
    }
    let runtime = RuntimePaths::for_state(&paths)?;
    let Some(store) = Store::open_existing(paths, runtime) else {
        return Ok(false);
    };
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    Ok(snapshot.agents.iter().any(|agent| {
        !agent.is_provider_subagent()
            && agent.ended_at.is_none()
            && agent.kind == target.kind
            && agent.agent_id == target.session
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

    fn task(prompt: &str) -> TaskEntry {
        TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some(prompt.to_owned()),
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
            once: Some(true),
            ..command.clone()
        };
        let timer = TaskEntry {
            watch: None,
            at: Some("12:00".into()),
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
                        at: Some("11:00".into()),
                        ..timer
                    },
                ),
                ("signal".into(), signal.clone()),
                (
                    "standing".into(),
                    TaskEntry {
                        once: None,
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
                        agent: Some("claude".into()),
                        ..command.clone()
                    },
                ),
            ])),
            Tasks(BTreeMap::from([("machine".into(), command)])),
            None,
            Some(Path::new("/repo")),
        );
        let session = (target.kind, target.session);
        assert_eq!(
            session_deliveries(&catalog, Path::new("/repo"), Some(&session))
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
        let waits = pending_waits_by_session(
            &catalog,
            Path::new("/repo"),
            &"2026-06-01T10:00:00Z[UTC]".parse().unwrap(),
        );
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
    fn foreign_machine_preserves_instance_and_both_overlay_keys() {
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
            assert!(catalog.overlay_keys.contains(&TaskKey::for_task(
                "same",
                source,
                &entry.resolved_root(),
            )));
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
            catalog.visible()["same"].entry.prompt.as_deref(),
            Some("project")
        );
        assert_eq!(
            catalog.for_run("same").unwrap().entry.prompt.as_deref(),
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
            catalog.visible()["same"].entry.prompt.as_deref(),
            Some("project")
        );
        assert_eq!(
            catalog.for_run("same").unwrap().entry.prompt.as_deref(),
            Some("project")
        );
        assert!(catalog.overlay_keys.contains("machine::same"));
        assert!(catalog.overlay_keys.contains(&TaskKey::for_task(
            "same",
            TaskSource::Project {
                state: TrustState::Trusted,
            },
            Path::new("/repo"),
        )));
    }

    #[test]
    fn empty_project_layer_keeps_its_overlay_scope() {
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
        entry.at = Some("07:00".to_owned());
        let catalog = TaskCatalog::from_layers(
            Tasks::default(),
            Tasks(BTreeMap::from([("broken".to_owned(), entry)])),
            None,
            None,
        );
        let loaded = &catalog.visible()["broken"];

        assert!(matches!(loaded.action(), Ok(TaskAction::Spawn(_))));
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
        assert!(!super::super::TaskShape::compile("task", &entry).is_ephemeral());
        entry.once = Some(true);
        assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());
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

        assert!(!delivery_target_alive(&entry, &target).expect("check vanished target"));
    }
}
