//! Rimz-owned loop task instances and merged loop task reads.
//!
//! Durable recurring definitions live in `loop.toml`. Machine-generated
//! one-shots, self-wakes, and poll-until instances live here as state, using
//! the same task entry shape without turning runtime churn into user config
//! edits. Readers merge both backings here; durable config wins when both
//! stores contain a name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{MachineConfig, TaskEntry, Tasks};
use crate::store::atomic::{Result, write_temp_then_rename_cache};
use crate::store::paths::state_home;
use crate::trust::TrustState;

const NAME: &str = "loop-instances.json";

pub fn path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

pub fn load() -> Tasks {
    load_from(&state_home())
}

pub fn load_all() -> BTreeMap<String, (TaskEntry, TaskSource)> {
    load_all_with_project(None)
}

pub fn load_entry(name: &str) -> Option<(TaskEntry, TaskSource)> {
    load_all().remove(name)
}

pub fn load_all_with_project(
    project: Option<(Tasks, TrustState)>,
) -> BTreeMap<String, (TaskEntry, TaskSource)> {
    load_all_from_layers(
        load(),
        MachineConfig::load_lenient().r#loop.tasks.clone(),
        trusted_project(project),
    )
}

pub fn load_entry_with_project(
    name: &str,
    project: Option<(Tasks, TrustState)>,
) -> Option<(TaskEntry, TaskSource)> {
    load_all_with_project(project).remove(name)
}

pub fn load_all_visible_with_project(
    project: Option<(Tasks, TrustState)>,
) -> BTreeMap<String, (TaskEntry, TaskSource)> {
    load_all_from_layers(
        load(),
        MachineConfig::load_lenient().r#loop.tasks.clone(),
        project,
    )
}

pub fn load_entry_visible_with_project(
    name: &str,
    project: Option<(Tasks, TrustState)>,
) -> Option<(TaskEntry, TaskSource)> {
    load_all_visible_with_project(project).remove(name)
}

pub fn is_ephemeral(entry: &TaskEntry) -> bool {
    entry.once || entry.deadline.is_some()
}

pub fn insert(name: &str, entry: &TaskEntry) -> Result<()> {
    insert_into(&state_home(), name, entry)
}

pub fn remove(name: &str) -> Result<bool> {
    remove_from(&state_home(), name)
}

pub fn rename(old: &str, new: &str) -> Result<bool> {
    rename_from(&state_home(), old, new)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskSource {
    Config,
    Instance,
    Project { state: TrustState },
}

impl TaskSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Instance => "state",
            Self::Project { .. } => "project",
        }
    }
}

fn load_from(state_root: &Path) -> Tasks {
    let Ok(bytes) = std::fs::read(path(state_root)) else {
        return Tasks::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

#[cfg(test)]
fn load_all_from(instances: Tasks, config: Tasks) -> BTreeMap<String, (TaskEntry, TaskSource)> {
    load_all_from_layers(instances, config, None)
}

fn load_all_from_layers(
    instances: Tasks,
    config: Tasks,
    project: Option<(Tasks, TrustState)>,
) -> BTreeMap<String, (TaskEntry, TaskSource)> {
    let mut tasks: BTreeMap<_, _> = instances
        .0
        .into_iter()
        .map(|(name, entry)| (name, (entry, TaskSource::Instance)))
        .collect();
    tasks.extend(
        config
            .0
            .into_iter()
            .map(|(name, entry)| (name, (entry, TaskSource::Config))),
    );
    if let Some((project, state)) = project {
        tasks.extend(
            project
                .0
                .into_iter()
                .map(|(name, entry)| (name, (entry, TaskSource::Project { state }))),
        );
    }
    tasks
}

fn trusted_project(project: Option<(Tasks, TrustState)>) -> Option<(Tasks, TrustState)> {
    let (tasks, state) = project?;
    (state == TrustState::Trusted).then_some((tasks, state))
}

fn insert_into(state_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    let mut tasks = load_from(state_root);
    tasks.0.insert(name.to_owned(), entry.clone());
    write_temp_then_rename_cache(&path(state_root), &tasks)
}

fn remove_from(state_root: &Path, name: &str) -> Result<bool> {
    let mut tasks = load_from(state_root);
    let removed = tasks.0.remove(name).is_some();
    if removed {
        write_temp_then_rename_cache(&path(state_root), &tasks)?;
    }
    Ok(removed)
}

fn rename_from(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    let mut tasks = load_from(state_root);
    let Some(entry) = tasks.0.remove(old) else {
        return Ok(false);
    };
    tasks.0.insert(new.to_owned(), entry);
    write_temp_then_rename_cache(&path(state_root), &tasks)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> TaskEntry {
        TaskEntry {
            spec: Some("claude".to_owned()),
            prompt: Some("wake".to_owned()),
            root: PathBuf::from("/repo"),
            at: Some("07:00".to_owned()),
            once: true,
            ..TaskEntry::default()
        }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(load_from(dir.path()).0.is_empty());
    }

    #[test]
    fn insert_and_remove_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert_into(dir.path(), "wake", &entry).expect("insert");
        assert_eq!(
            load_from(dir.path()).0.get("wake").map(|entry| entry.once),
            Some(true)
        );

        assert!(remove_from(dir.path(), "wake").expect("remove"));
        assert!(load_from(dir.path()).0.is_empty());
        assert!(!remove_from(dir.path(), "wake").expect("remove absent"));
    }

    #[test]
    fn rename_moves_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert_into(dir.path(), "wake", &entry).expect("insert");

        assert!(rename_from(dir.path(), "wake", "nudge").expect("rename"));
        let tasks = load_from(dir.path());
        assert!(!tasks.0.contains_key("wake"));
        assert_eq!(
            tasks.0.get("nudge").map(|entry| entry.prompt.as_deref()),
            Some(Some("wake"))
        );
        assert!(!rename_from(dir.path(), "wake", "later").expect("rename absent"));
    }

    #[test]
    fn merged_reads_prefer_config_over_instance() {
        let mut instance = task();
        instance.prompt = Some("state".to_owned());
        let mut config = task();
        config.prompt = Some("config".to_owned());

        let tasks = load_all_from(
            Tasks(BTreeMap::from([("wake".to_owned(), instance)])),
            Tasks(BTreeMap::from([("wake".to_owned(), config)])),
        );

        let (entry, source) = tasks.get("wake").expect("wake task");
        assert_eq!(source, &TaskSource::Config);
        assert_eq!(entry.prompt.as_deref(), Some("config"));
    }

    #[test]
    fn merged_reads_prefer_project_over_config_over_instance() {
        let mut instance = task();
        instance.prompt = Some("state".to_owned());
        let mut config = task();
        config.prompt = Some("config".to_owned());
        let mut project = task();
        project.prompt = Some("project".to_owned());

        let tasks = load_all_from_layers(
            Tasks(BTreeMap::from([("wake".to_owned(), instance)])),
            Tasks(BTreeMap::from([("wake".to_owned(), config)])),
            Some((
                Tasks(BTreeMap::from([("wake".to_owned(), project)])),
                TrustState::Trusted,
            )),
        );

        let (entry, source) = tasks.get("wake").expect("wake task");
        assert_eq!(
            source,
            &TaskSource::Project {
                state: TrustState::Trusted
            }
        );
        assert_eq!(entry.prompt.as_deref(), Some("project"));
    }

    #[test]
    fn untrusted_project_tasks_do_not_enter_effective_merge() {
        let mut config = task();
        config.prompt = Some("config".to_owned());
        let mut project = task();
        project.prompt = Some("project".to_owned());

        let tasks = load_all_from_layers(
            Tasks::default(),
            Tasks(BTreeMap::from([("wake".to_owned(), config)])),
            trusted_project(Some((
                Tasks(BTreeMap::from([("wake".to_owned(), project)])),
                TrustState::Untrusted,
            ))),
        );

        let (entry, source) = tasks.get("wake").expect("wake task");
        assert_eq!(source, &TaskSource::Config);
        assert_eq!(entry.prompt.as_deref(), Some("config"));
    }

    #[test]
    fn visible_reads_show_untrusted_project_tasks() {
        let mut config = task();
        config.prompt = Some("config".to_owned());
        let mut project = task();
        project.prompt = Some("project".to_owned());

        let tasks = load_all_from_layers(
            Tasks::default(),
            Tasks(BTreeMap::from([("wake".to_owned(), config)])),
            Some((
                Tasks(BTreeMap::from([("wake".to_owned(), project)])),
                TrustState::Untrusted,
            )),
        );

        let (entry, source) = tasks.get("wake").expect("wake task");
        assert_eq!(
            source,
            &TaskSource::Project {
                state: TrustState::Untrusted
            }
        );
        assert_eq!(entry.prompt.as_deref(), Some("project"));
    }

    #[test]
    fn ephemeral_tasks_are_once_or_deadline_bound() {
        let mut entry = task();
        entry.once = false;
        entry.deadline = None;
        assert!(!is_ephemeral(&entry));

        entry.once = true;
        assert!(is_ephemeral(&entry));

        entry.once = false;
        entry.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
        assert!(is_ephemeral(&entry));
    }
}
