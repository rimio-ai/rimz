//! Comment-preserving TOML task stores for machine `loop.toml` and project `.rimz/config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use crate::store::atomic::write_bytes_atomically;
use crate::store::paths::agents_home;
use crate::trust::TrustState;

const PROJECT_CONFIG_REL: &str = ".rimz/config.toml";

#[derive(Clone, Copy, Debug)]
pub enum TaskStore<'a> {
    Machine,
    Project(&'a Path),
}

impl TaskStore<'_> {
    pub fn path(self) -> PathBuf {
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

pub fn set_entry(store: TaskStore<'_>, name: &str, entry: &TaskEntry) -> Result<()> {
    let path = store.path();
    let mut doc = store
        .read_text()?
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    root_tasks_table(&mut doc)?.insert(
        name,
        Item::Table(task_entry_table(entry, matches!(store, TaskStore::Machine))),
    );

    let rendered = doc.to_string();
    store.validate(&rendered, name)?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn remove(store: TaskStore<'_>, name: &str) -> Result<bool> {
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

pub fn rename(store: TaskStore<'_>, name: &str, new_name: &str) -> Result<bool> {
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

fn task_entry_table(entry: &TaskEntry, include_root: bool) -> Table {
    let mut table = Table::new();
    if let Some(agent) = &entry.agent {
        table["agent"] = value(agent);
    }
    if let Some(target) = &entry.wake {
        table["wake"] = Item::Table(task_target_table(target));
    }
    if let Some(prompt) = &entry.prompt {
        table["prompt"] = value(prompt);
    }
    if let Some(prompt_file) = &entry.prompt_file {
        table["prompt-file"] = value(prompt_file.to_string_lossy().into_owned());
    }
    if let Some(check) = &entry.check {
        table["check"] = value(check);
    }
    if let Some(verify) = &entry.verify {
        table["verify"] = value(verify);
    }
    if let Some(max_attempts) = entry.max_attempts {
        table["max-attempts"] = value(i64::from(max_attempts));
    }
    if let Some(max_strikes) = entry.max_strikes {
        table["max-strikes"] = value(i64::from(max_strikes));
    }
    if let Some(on) = entry.on {
        table["on"] = value(match on {
            CheckOn::Fail => "fail",
            CheckOn::Success => "success",
        });
    }
    if include_root {
        table["root"] = value(entry.root.to_string_lossy().into_owned());
    }
    if let Some(worktree) = &entry.worktree {
        table["worktree"] = value(worktree);
    }
    if let Some(mode) = &entry.mode {
        table["mode"] = value(mode);
    }
    if let Some(effort) = &entry.effort {
        table["effort"] = value(effort);
    }
    if let Some(budget) = &entry.budget {
        table["budget"] = value(budget);
    }
    if let Some(budget) = &entry.budget_per_day {
        table["budget-per-day"] = value(budget);
    }
    if let Some(surplus) = &entry.surplus {
        table["surplus"] = value(surplus);
    }
    if let Some(after) = &entry.surplus_after {
        table["surplus-after"] = value(after);
    }
    if let Some(path) = &entry.system_prompt_file {
        table["system-prompt-file"] = value(path.to_string_lossy().into_owned());
    }
    if let Some(timeout) = &entry.timeout {
        table["timeout"] = value(timeout);
    }
    if let Some(at) = &entry.at {
        table["at"] = value(at);
    }
    if let Some(every) = &entry.every {
        table["every"] = value(every);
    }
    if let Some(cron) = &entry.cron {
        table["cron"] = value(cron);
    }
    if let Some(deadline) = entry.deadline {
        table["deadline"] = value(deadline.to_string());
    }
    table
}

fn task_target_table(target: &TaskTarget) -> Table {
    let mut table = Table::new();
    table["kind"] = value(target.kind.as_str());
    table["session"] = value(target.session.as_str());
    table["handle"] = value(target.handle.as_str());
    table
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

    #[test]
    fn task_entry_table_persists_surplus_gate_fields() {
        let table = task_entry_table(
            &TaskEntry {
                surplus: Some("1.5x".to_owned()),
                surplus_after: Some("3d".to_owned()),
                ..TaskEntry::default()
            },
            true,
        );

        assert_eq!(table["surplus"].as_str(), Some("1.5x"));
        assert_eq!(table["surplus-after"].as_str(), Some("3d"));
    }
}
