//! Comment-preserving TOML task stores for machine `loop.toml` and project `.rimz/config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Table};

use crate::config::{MachineConfig, TaskEntry};
use crate::store::atomic::write_bytes_atomically;
use crate::store::paths::agents_home;
use crate::trust::TrustState;

const PROJECT_CONFIG_REL: &str = ".rimz/config.toml";

#[derive(Clone, Copy, Debug)]
pub(super) enum TaskStore<'a> {
    Machine,
    Project(&'a Path),
}

impl TaskStore<'_> {
    pub(super) fn path(self) -> PathBuf {
        match self {
            Self::Machine => MachineConfig::loop_path(),
            Self::Project(project_root) => project_root.join(PROJECT_CONFIG_REL),
        }
    }

    fn read_text(self) -> Result<String> {
        let path = self.path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(match self {
                Self::Machine => MachineConfig::template_loop().to_owned(),
                Self::Project(_) => String::new(),
            }),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn validate(self, rendered: &str, name: &str) -> Result<()> {
        let path = self.path();
        match self {
            Self::Machine => {
                MachineConfig::parse_text(&path, rendered, &agents_home())
                    .with_context(|| format!("validating `loop.tasks.{name}`"))?;
            }
            Self::Project(project_root) => {
                let value = toml::from_str::<toml::Value>(rendered)
                    .with_context(|| format!("parsing {}", path.display()))?;
                crate::config::effective::project_tasks_from_value(
                    project_root,
                    &path,
                    TrustState::Untrusted,
                    &value,
                )
                .with_context(|| format!("validating project `tasks.{name}`"))?;
            }
        }
        Ok(())
    }
}

pub(super) fn set_entry(store: TaskStore<'_>, name: &str, entry: &TaskEntry) -> Result<()> {
    let path = store.path();
    let mut doc = store
        .read_text()?
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    root_tasks_table(&mut doc)?.insert(
        name,
        Item::Table(task_entry_table(
            entry,
            matches!(store, TaskStore::Machine),
        )?),
    );

    let rendered = doc.to_string();
    store.validate(&rendered, name)?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub(super) fn remove(store: TaskStore<'_>, name: &str) -> Result<bool> {
    let path = store.path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let removed = doc
        .get_mut("tasks")
        .and_then(Item::as_table_mut)
        .map(|tasks| tasks.remove(name).is_some())
        .unwrap_or(false);
    if removed {
        let rendered = doc.to_string();
        if matches!(store, TaskStore::Project(_)) {
            store.validate(&rendered, name)?;
        }
        write_bytes_atomically(&path, rendered.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(removed)
}

pub(super) fn rename(store: TaskStore<'_>, name: &str, new_name: &str) -> Result<bool> {
    let path = store.path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let Some(tasks) = doc.get_mut("tasks").and_then(Item::as_table_mut) else {
        return Ok(false);
    };
    if tasks.contains_key(new_name) {
        bail!("loop task `{new_name}` already exists");
    }
    let Some(entry) = tasks.remove(name) else {
        return Ok(false);
    };
    tasks.insert(new_name, entry);

    let rendered = doc.to_string();
    store.validate(&rendered, new_name)?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

fn task_entry_table(entry: &TaskEntry, include_root: bool) -> Result<Table> {
    // Serde rejects non-UTF-8 paths; config text historically persisted their
    // lossy display form, so normalize only path values before serialization.
    let mut serializable = entry.clone();
    serializable.root = PathBuf::from(entry.root.to_string_lossy().into_owned());
    serializable.prompt_file = entry
        .prompt_file
        .as_deref()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()));
    serializable.system_prompt_file = entry
        .system_prompt_file
        .as_deref()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()));
    let mut table = toml_edit::ser::to_document(&serializable)
        .context("serializing loop task")?
        .into_table();
    if !include_root {
        table.remove("root");
    }
    if let Some(wake) = table.remove("wake") {
        table.insert(
            "wake",
            Item::Table(
                wake.into_table()
                    .map_err(|_| anyhow::anyhow!("serialized loop wake is not a table"))?,
            ),
        );
    }
    Ok(table)
}

fn root_tasks_table(doc: &mut DocumentMut) -> Result<&mut Table> {
    let tasks = doc
        .as_table_mut()
        .entry("tasks")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("`tasks` is not a table")?;
    tasks.set_implicit(true);
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CheckOn, TaskTarget};

    #[test]
    fn task_entry_serialization_uses_canonical_schema_and_store_policy() {
        let entry = TaskEntry {
            agent: Some("claude".to_owned()),
            wake: Some(TaskTarget {
                kind: "claude".to_owned(),
                session: "session-1".to_owned(),
                handle: "@claude".to_owned(),
            }),
            prompt: Some("wake".to_owned()),
            prompt_file: Some(PathBuf::from("prompts/wake.md")),
            check: Some("cargo check".to_owned()),
            verify: Some("cargo xtask gate".to_owned()),
            max_attempts: Some(3),
            max_strikes: Some(4),
            on: Some(CheckOn::Success),
            root: PathBuf::from("/repo"),
            worktree: Some("task".to_owned()),
            mode: Some("plan".to_owned()),
            effort: Some("high".to_owned()),
            budget: Some("$2".to_owned()),
            budget_per_day: Some("$8".to_owned()),
            surplus: Some("1.5x".to_owned()),
            surplus_after: Some("3d".to_owned()),
            system_prompt_file: Some(PathBuf::from("prompts/system.md")),
            timeout: Some("20m".to_owned()),
            at: Some("07:00".to_owned()),
            every: Some("1h".to_owned()),
            cron: Some("0 * * * *".to_owned()),
            deadline: Some(jiff::Timestamp::UNIX_EPOCH),
        };

        let machine = task_entry_table(&entry, true).expect("serialize machine");
        let mut doc = DocumentMut::new();
        root_tasks_table(&mut doc)
            .expect("tasks table")
            .insert("full", Item::Table(machine.clone()));
        let machine_text = doc.to_string();
        assert!(machine.contains_key("root"));
        assert!(machine.contains_key("prompt-file"));
        assert!(machine.contains_key("max-attempts"));
        assert!(machine.contains_key("budget-per-day"));
        assert!(machine.contains_key("system-prompt-file"));
        assert!(machine_text.contains("[tasks.full.wake]"));
        assert_eq!(
            toml_edit::de::from_document::<TaskEntry>(DocumentMut::from(machine))
                .expect("machine round trip"),
            entry
        );

        let project = task_entry_table(&entry, false).expect("serialize project");
        assert!(!project.contains_key("root"));
        let mut project_round =
            toml_edit::de::from_document::<TaskEntry>(DocumentMut::from(project))
                .expect("project round trip");
        project_round.root = entry.root.clone();
        assert_eq!(project_round, entry);
    }

    #[cfg(unix)]
    #[test]
    fn task_entry_serialization_lossy_encodes_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(b"/repo/\xff".to_vec()));
        let entry = TaskEntry {
            root: path.clone(),
            prompt_file: Some(path.clone()),
            system_prompt_file: Some(path.clone()),
            ..TaskEntry::default()
        };

        let table = task_entry_table(&entry, true).expect("serialize lossy paths");
        let expected = path.to_string_lossy();
        assert_eq!(table["root"].as_str(), Some(expected.as_ref()));
        assert_eq!(table["prompt-file"].as_str(), Some(expected.as_ref()));
        assert_eq!(
            table["system-prompt-file"].as_str(),
            Some(expected.as_ref())
        );
    }
}
