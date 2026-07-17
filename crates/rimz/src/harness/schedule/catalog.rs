//! Loop task catalog, source precedence, and coordinated mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{TaskAction, config_edit, instances, pauses, strikes};
use crate::Store;
use crate::config::{MachineConfig, TaskEntry, Tasks};
use crate::store::paths::{RuntimePaths, StatePaths, config_home, state_home};
use crate::trust::TrustState;
use crate::workspace::WorkspaceResolver;

pub fn is_ephemeral(entry: &TaskEntry) -> bool {
    (entry.every.is_none() && entry.cron.is_none()) || entry.deadline.is_some()
}

#[doc(hidden)]
pub fn instances_path(state_root: &Path) -> PathBuf {
    instances::path(state_root)
}

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
            Self::Instance => instances::path(&state_home()),
            Self::Project { .. } => config_edit::TaskStore::Project(&entry.root).path(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedTask {
    pub entry: TaskEntry,
    pub source: TaskSource,
    synthetic: bool,
}

impl LoadedTask {
    pub fn action(&self, name: &str) -> Result<TaskAction, super::TaskActionErr> {
        TaskAction::from_entry(name, &self.entry)
    }
}

#[derive(Clone, Debug)]
pub struct TaskCatalog {
    visible: BTreeMap<String, LoadedTask>,
    runnable: BTreeMap<String, LoadedTask>,
}

impl TaskCatalog {
    /// Strict interactive load. Malformed machine, instance, or project state
    /// fails at command entry.
    pub fn load(project_root: Option<&Path>) -> Result<Self> {
        let instances = instances::load_strict_from(&state_home())?;
        let machine = MachineConfig::load_loop().context("reading per-machine loop.toml")?;
        let auto_ping = machine.auto_ping;
        let machine_tasks = machine.tasks;
        let project = project_root
            .map(|root| crate::config::effective::project_tasks(root, &config_home()))
            .transpose()?
            .flatten()
            .map(|project| (project.tasks, project.state));
        Ok(Self::from_layers(instances, machine_tasks, project)
            .with_auto_ping(auto_ping, project_root))
    }

    /// Best-effort load for elder, doctor, and maintenance reads.
    pub fn load_lenient(project_root: Option<&Path>) -> Self {
        let instances = instances::load();
        let machine = MachineConfig::load_lenient().r#loop.clone();
        let auto_ping = machine.auto_ping;
        let project = project_root.and_then(|root| {
            crate::config::effective::project_tasks(root, &config_home())
                .ok()
                .flatten()
                .map(|project| (project.tasks, project.state))
        });
        Self::from_layers(instances, machine.tasks, project).with_auto_ping(auto_ping, project_root)
    }

    fn from_layers(instances: Tasks, machine: Tasks, project: Option<(Tasks, TrustState)>) -> Self {
        let mut runnable = merge_base(instances, machine);
        let mut visible = runnable.clone();
        if let Some((project, state)) = project {
            let project = project.0.into_iter().map(|(name, entry)| {
                (
                    name,
                    LoadedTask {
                        entry,
                        source: TaskSource::Project { state },
                        synthetic: false,
                    },
                )
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
        Self { visible, runnable }
    }

    fn with_auto_ping(mut self, enabled: bool, project_root: Option<&Path>) -> Self {
        let Some(project_root) = project_root.filter(|_| enabled) else {
            return self;
        };
        for adapter in crate::agents::registry::ADAPTERS {
            if adapter.ping_args().is_none() {
                continue;
            }
            let kind = adapter.descriptor().kind;
            let name = format!("autoping-{kind}");
            if self.visible.contains_key(&name) {
                continue;
            }
            let task = LoadedTask {
                entry: TaskEntry {
                    agent: Some(format!("{kind}-ping")),
                    prompt: Some("ping".to_owned()),
                    root: project_root.to_path_buf(),
                    every: Some("reset".to_owned()),
                    ..TaskEntry::default()
                },
                source: TaskSource::Config,
                synthetic: true,
            };
            self.visible.insert(name.clone(), task.clone());
            self.runnable.insert(name, task);
        }
        self
    }

    pub fn visible(&self) -> &BTreeMap<String, LoadedTask> {
        &self.visible
    }

    pub fn for_run(&self, name: &str) -> Option<&LoadedTask> {
        self.runnable.get(name)
    }

    pub(crate) fn runnable(&self) -> &BTreeMap<String, LoadedTask> {
        &self.runnable
    }

    pub fn replace_machine(&self, name: &str, entry: &TaskEntry) -> Result<TaskMutation> {
        if matches!(
            self.visible.get(name).map(|task| task.source),
            Some(TaskSource::Project { .. })
        ) {
            bail!(
                "loop task `{name}` is project-owned in {}; use `rimz loop add --project` or choose another name",
                config_edit::TaskStore::Project(&entry.root)
                    .path()
                    .display()
            );
        }
        if is_ephemeral(entry) {
            config_edit::remove(config_edit::TaskStore::Machine, name)?;
            instances::insert(name, entry)?;
        } else {
            instances::remove(name)?;
            config_edit::set_entry(config_edit::TaskStore::Machine, name, entry)?;
        }
        clear_overlays(name, TaskSource::from_entry(entry), None)
    }

    pub fn replace_project(
        &self,
        name: &str,
        project_root: &Path,
        entry: &TaskEntry,
    ) -> Result<TaskMutation> {
        config_edit::set_entry(config_edit::TaskStore::Project(project_root), name, entry)?;
        clear_overlays(
            name,
            TaskSource::Project {
                state: crate::trust::status(project_root)?.state,
            },
            Some(project_root.to_path_buf()),
        )
    }

    pub fn remove(&self, name: &str) -> Result<TaskMutation> {
        let Some(task) = self.visible.get(name) else {
            return Ok(TaskMutation::unchanged());
        };
        refuse_synthetic_mutation(name, task)?;
        let changed = remove_definition(name, task)?;
        let cleared_pause = pauses::remove(name)?;
        let cleared_strikes = strikes::clear(name)?;
        Ok(TaskMutation {
            changed,
            source: Some(task.source),
            project_root: project_root(task),
            cleared_pause,
            cleared_strikes,
        })
    }

    pub fn rename(&self, name: &str, new_name: &str) -> Result<TaskMutation> {
        if self.visible.contains_key(new_name) {
            bail!("loop task `{new_name}` already exists");
        }
        let Some(task) = self.visible.get(name) else {
            return Ok(TaskMutation::unchanged());
        };
        refuse_synthetic_mutation(name, task)?;
        let changed = match task.source {
            TaskSource::Config => {
                config_edit::rename(config_edit::TaskStore::Machine, name, new_name)?
            }
            TaskSource::Instance => instances::rename(name, new_name)?,
            TaskSource::Project { .. } => config_edit::rename(
                config_edit::TaskStore::Project(&task.entry.root),
                name,
                new_name,
            )?,
        };
        let cleared_pause = pauses::rename(name, new_name)?;
        let cleared_strikes = strikes::rename(name, new_name)?;
        Ok(TaskMutation {
            changed,
            source: Some(task.source),
            project_root: project_root(task),
            cleared_pause,
            cleared_strikes,
        })
    }

    /// Remove only the definition selected by catalog precedence. Pause and
    /// strike overlays stay untouched because this is scheduled consumption,
    /// not an interactive edit.
    pub fn consume_scheduled(&self, name: &str) -> Result<TaskMutation> {
        let Some(task) = self.runnable.get(name) else {
            return Ok(TaskMutation::unchanged());
        };
        Ok(TaskMutation {
            changed: remove_definition(name, task)?,
            source: Some(task.source),
            project_root: project_root(task),
            cleared_pause: false,
            cleared_strikes: false,
        })
    }

    pub fn prune_orphan_overlays(&self) -> Result<usize> {
        let known = self.visible.keys().cloned().collect::<BTreeSet<_>>();
        Ok(pauses::prune_orphans(&known)? + strikes::prune_orphans(&known)?)
    }

    pub fn reap_dead_deliveries() -> Result<usize> {
        let catalog = Self::load_lenient(None);
        let mut reaped = 0;
        for (name, task) in catalog.runnable.clone() {
            let target = match task.action(&name) {
                Ok(TaskAction::Deliver(target)) => target,
                Ok(TaskAction::Spawn(_) | TaskAction::CheckOnly) => continue,
                Err(err) => {
                    tracing::debug!(task = %name, error = %err, "invalid loop task skipped by schedule gc");
                    continue;
                }
            };
            match delivery_target_alive(&task.entry, &target) {
                Ok(true) => {}
                Ok(false) => {
                    catalog.consume_scheduled(&name)?;
                    reaped += 1;
                }
                Err(err) => {
                    tracing::debug!(task = %name, error = %err, "loop schedule gc skipped task");
                }
            }
        }
        Ok(reaped)
    }
}

impl TaskSource {
    fn from_entry(entry: &TaskEntry) -> Self {
        if is_ephemeral(entry) {
            Self::Instance
        } else {
            Self::Config
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskMutation {
    changed: bool,
    source: Option<TaskSource>,
    project_root: Option<PathBuf>,
    cleared_pause: bool,
    cleared_strikes: bool,
}

impl TaskMutation {
    fn unchanged() -> Self {
        Self::default()
    }

    pub const fn changed(&self) -> bool {
        self.changed
    }

    pub const fn source(&self) -> Option<TaskSource> {
        self.source
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    pub const fn cleared_pause(&self) -> bool {
        self.cleared_pause
    }

    pub const fn cleared_strikes(&self) -> bool {
        self.cleared_strikes
    }

    pub const fn cleared_overlays(&self) -> bool {
        self.cleared_pause || self.cleared_strikes
    }
}

fn merge_base(instances: Tasks, machine: Tasks) -> BTreeMap<String, LoadedTask> {
    let mut tasks = instances
        .0
        .into_iter()
        .map(|(name, entry)| {
            (
                name,
                LoadedTask {
                    entry,
                    source: TaskSource::Instance,
                    synthetic: false,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    tasks.extend(machine.0.into_iter().map(|(name, entry)| {
        (
            name,
            LoadedTask {
                entry,
                source: TaskSource::Config,
                synthetic: false,
            },
        )
    }));
    tasks
}

fn refuse_synthetic_mutation(name: &str, task: &LoadedTask) -> Result<()> {
    if task.synthetic {
        bail!(
            "loop task `{name}` is generated by `auto-ping`; pause it with `rimz loop pause {name}` or disable generated pings with `rimz config set loop.auto-ping false`"
        );
    }
    Ok(())
}

fn clear_overlays(
    name: &str,
    source: TaskSource,
    project_root: Option<PathBuf>,
) -> Result<TaskMutation> {
    Ok(TaskMutation {
        changed: true,
        source: Some(source),
        project_root,
        cleared_pause: pauses::remove(name)?,
        cleared_strikes: strikes::clear(name)?,
    })
}

fn project_root(task: &LoadedTask) -> Option<PathBuf> {
    matches!(task.source, TaskSource::Project { .. }).then(|| task.entry.root.clone())
}

fn remove_definition(name: &str, task: &LoadedTask) -> Result<bool> {
    match task.source {
        TaskSource::Config => Ok(config_edit::remove(config_edit::TaskStore::Machine, name)?),
        TaskSource::Instance => Ok(instances::remove(name)?),
        TaskSource::Project { .. } => Ok(config_edit::remove(
            config_edit::TaskStore::Project(&task.entry.root),
            name,
        )?),
    }
}

pub fn delivery_target_alive(
    entry: &TaskEntry,
    target: &crate::config::TaskTarget,
) -> Result<bool> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone())?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id)?;
    let store = Store::open(paths, runtime)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    Ok(snapshot.agents.iter().any(|agent| {
        agent.parent_agent_id.is_none()
            && agent.kind.as_str() == target.kind.as_str()
            && agent.agent_id.as_str() == target.session
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        );

        assert_eq!(
            catalog.visible()["same"].entry.prompt.as_deref(),
            Some("project")
        );
        assert_eq!(
            catalog.for_run("same").unwrap().entry.prompt.as_deref(),
            Some("project")
        );
    }

    #[test]
    fn auto_ping_synthesizes_each_ping_capable_adapter_with_a_root() {
        let catalog = TaskCatalog::from_layers(Tasks::default(), Tasks::default(), None)
            .with_auto_ping(true, Some(Path::new("/repo")));
        let expected = crate::agents::registry::ADAPTERS
            .iter()
            .filter(|adapter| adapter.ping_args().is_some())
            .map(|adapter| format!("autoping-{}", adapter.descriptor().kind))
            .collect::<BTreeSet<_>>();

        assert_eq!(
            catalog.visible.keys().cloned().collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(
            catalog.runnable.keys().cloned().collect::<BTreeSet<_>>(),
            expected
        );
        assert!(catalog.visible.contains_key("autoping-qwen"));
        for (name, task) in &catalog.visible {
            let kind = name.trim_start_matches("autoping-");
            assert_eq!(task.source, TaskSource::Config);
            assert_eq!(
                task.entry.agent.as_deref(),
                Some(format!("{kind}-ping").as_str())
            );
            assert_eq!(task.entry.prompt.as_deref(), Some("ping"));
            assert_eq!(task.entry.root, Path::new("/repo"));
            assert_eq!(task.entry.worktree, None);
            assert_eq!(
                task.action(name).unwrap(),
                TaskAction::Spawn(format!("{kind}-ping"))
            );
            assert!(matches!(
                super::super::parse_schedule(name, &task.entry)
                    .unwrap()
                    .schedule,
                super::super::Schedule::WindowReset
            ));
        }
    }

    #[test]
    fn auto_ping_requires_enablement_and_a_project_root() {
        let disabled = TaskCatalog::from_layers(Tasks::default(), Tasks::default(), None)
            .with_auto_ping(false, Some(Path::new("/repo")));
        let rootless = TaskCatalog::from_layers(Tasks::default(), Tasks::default(), None)
            .with_auto_ping(true, None);

        assert!(disabled.visible.is_empty());
        assert!(rootless.visible.is_empty());
    }

    #[test]
    fn user_task_shadows_synthetic_auto_ping() {
        let user = task("user-owned");
        let catalog = TaskCatalog::from_layers(
            Tasks::default(),
            Tasks(BTreeMap::from([("autoping-claude".to_owned(), user)])),
            None,
        )
        .with_auto_ping(true, Some(Path::new("/repo")));

        assert_eq!(
            catalog.visible["autoping-claude"].entry.prompt.as_deref(),
            Some("user-owned")
        );
        assert_eq!(
            catalog.runnable["autoping-claude"].entry.prompt.as_deref(),
            Some("user-owned")
        );
    }

    #[test]
    fn synthetic_auto_ping_refuses_definition_mutations() {
        let catalog = TaskCatalog::from_layers(Tasks::default(), Tasks::default(), None)
            .with_auto_ping(true, Some(Path::new("/repo")));

        for error in [
            catalog.remove("autoping-claude").unwrap_err(),
            catalog
                .rename("autoping-claude", "renamed-primer")
                .unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("generated by `auto-ping`"), "{message}");
            assert!(
                message.contains("rimz loop pause autoping-claude"),
                "{message}"
            );
            assert!(message.contains("loop.auto-ping false"), "{message}");
        }
    }

    #[test]
    fn ephemeral_tasks_are_one_shot_or_deadline_bound() {
        let mut entry = task("wake");
        assert!(!is_ephemeral(&entry));

        entry.every = None;
        assert!(is_ephemeral(&entry));

        entry.every = Some("15m".to_owned());
        entry.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
        assert!(is_ephemeral(&entry));
    }
}
