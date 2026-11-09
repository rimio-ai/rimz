//! `rimz loop eject`: publish a task's compiled graph as a deployed workflow and point the
//! task's row at it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use super::catalog::{TaskCatalog, TaskSource};
use super::compile::{CompiledAction, TaskWorkflow};
use super::config_edit::{self, TaskStore};
use crate::config::definitions::LoadedDefinitions;
use crate::config::{MachineConfig, TaskEntry};
use crate::disk::paths::{workflow_spec_file, workflows_dir};
use crate::harness::workflow::InlineDefinition;
use crate::trust::TrustState;

impl TaskCatalog {
    /// Publish task `name`'s compiled graph as the deployed workflow `workflow`, then rewrite
    /// the task's row in its own store to `workflow = "<workflow>"`. A directory that already
    /// holds exactly this definition is an earlier eject the rewrite never followed, so the
    /// rewrite finishes; any other existing directory refuses.
    pub fn eject(
        &self,
        name: &str,
        workflow: &str,
        config: &MachineConfig,
        definitions: &LoadedDefinitions,
    ) -> Result<PathBuf> {
        super::validate_name(workflow)?;
        let task = self
            .visible()
            .get(name)
            .ok_or_else(|| anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
        let entry = task.entry();
        let store = match task.source() {
            TaskSource::Config => TaskStore::Machine,
            TaskSource::Project {
                state: TrustState::Trusted,
            } => TaskStore::Project(&entry.root),
            // Its commands would run from the machine's workflows, which no project trust gates.
            TaskSource::Project { .. } => bail!(
                "loop task `{name}` belongs to a project that is not trusted; review it with \
                 `rimz trust` and run `rimz trust grant` in {} first",
                entry.root.display()
            ),
            TaskSource::Instance => bail!(
                "loop task `{name}` is a room-instance row (a wait or one-shot bound to this \
                 room's session); it cannot outlive that session, so it has no workflow to eject"
            ),
        };
        if entry.fresh_worktree.is_some() {
            bail!(
                "loop task `{name}` sets `fresh-worktree`, which a workflow task's row refuses; \
                 a workflow's steps place their own agents, so remove `fresh-worktree` or keep \
                 the task inline"
            );
        }
        let inline = match task.compiled().map_err(Clone::clone)? {
            CompiledAction::Workflow(TaskWorkflow::Inline(inline)) => inline,
            CompiledAction::Workflow(TaskWorkflow::Named(named)) => bail!(
                "loop task `{name}` already runs workflow `{named}`; edit that workflow instead"
            ),
            CompiledAction::Deliver(_) => {
                bail!("loop task `{name}` delivers to a living agent and runs no workflow")
            }
        };
        let definition = inline.definition(name, task, config)?;
        let project_root = entry.resolved_root();
        let home = workflows_dir();
        let dir = home.join(workflow);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => {
                if read_tree(&dir)? != files(&definition) {
                    bail!(
                        "workflow `{workflow}` already exists at {}; choose another name with \
                         `rimz loop eject {name} <workflow-name>`",
                        dir.display()
                    );
                }
                validate(&dir, definitions, &project_root)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                publish(&home, &dir, &definition, definitions, &project_root)?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", dir.display()));
            }
        }
        config_edit::set_entry(store, name, &ejected_row(entry, workflow))?;
        Ok(dir)
    }
}

/// The row once its graph and launch settings live in the workflow: what still decides when
/// and how often it fires, and what admits each run.
fn ejected_row(entry: &TaskEntry, workflow: &str) -> TaskEntry {
    TaskEntry {
        workflow: Some(workflow.to_owned()),
        once_per_checkout: Some(entry.dedupes_checkout()),
        agent: None,
        stay: false,
        takeover: false,
        subscribe: Vec::new(),
        prompt: None,
        prompt_file: None,
        notes: None,
        check: None,
        check_timeout: None,
        judge: Vec::new(),
        verify: None,
        max_attempts: None,
        on: None,
        worktree: None,
        mode: None,
        effort: None,
        fast: None,
        budget: None,
        system_prompt_file: None,
        timeout: None,
        account: None,
        throttle: None,
        capacity_wait: None,
        ..entry.clone()
    }
}

/// Every file the definition publishes, by path relative to its directory.
fn files(definition: &InlineDefinition) -> BTreeMap<PathBuf, Vec<u8>> {
    definition
        .assets
        .iter()
        .map(|(name, body)| (PathBuf::from(name), body.clone()))
        .chain([(
            workflow_spec_file(Path::new("")),
            definition.text.clone().into_bytes(),
        )])
        .collect()
}

fn read_tree(dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        let entries =
            std::fs::read_dir(&next).with_context(|| format!("reading {}", next.display()))?;
        for entry in entries {
            let path = entry
                .with_context(|| format!("reading {}", next.display()))?
                .path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let body =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            // `read_dir` yields paths under `dir`, so the prefix always strips.
            let relative = path.strip_prefix(dir).unwrap_or(&path).to_owned();
            found.insert(relative, body);
        }
    }
    Ok(found)
}

/// Write the definition into a staging directory beside `dir`, validate it there, and rename
/// it into place, so the workflow appears whole or not at all.
fn publish(
    home: &Path,
    dir: &Path,
    definition: &InlineDefinition,
    definitions: &LoadedDefinitions,
    project_root: &Path,
) -> Result<()> {
    let name = dir
        .file_name()
        .ok_or_else(|| anyhow!("workflow directory needs a name"))?;
    // Validation reads the directory's name as the workflow's, so the staging copy keeps it,
    // under a dot directory no workflow name can take; the next eject replaces a crashed one.
    let parent = home.join(".eject");
    let staging = parent.join(name);
    match std::fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("removing {}", staging.display()));
        }
    }
    crate::store::workflow::seal_definition(&staging, &definition.text, &definition.assets)?;
    let published = validate(&staging, definitions, project_root).and_then(|()| {
        std::fs::rename(&staging, dir).with_context(|| format!("publishing {}", dir.display()))?;
        Ok(crate::disk::atomic::sync_dir(home)?)
    });
    if published.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    // Another eject may be staging beside this one; it removes the directory itself.
    let _ = std::fs::remove_dir(&parent);
    published
}

fn validate(dir: &Path, definitions: &LoadedDefinitions, project_root: &Path) -> Result<()> {
    let report = crate::harness::workflow::validate(dir, definitions, Some(project_root));
    if report.errors.is_empty() {
        return Ok(());
    }
    let errors = report
        .errors
        .iter()
        .map(|issue| format!("{}: {}", issue.path, issue.message))
        .collect::<Vec<_>>();
    bail!(
        "the task's compiled workflow does not validate:\n{}",
        errors.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TaskEntry;
    use crate::disk::paths::StatePaths;

    /// A project store holding `text`. Unit tests share one RimZ home, so they never write
    /// the machine store.
    fn project(text: &str, trusted: bool) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".rimz")).unwrap();
        std::fs::write(root.path().join(".rimz/config.toml"), text).unwrap();
        if trusted {
            crate::trust::grant(root.path()).unwrap();
        }
        root
    }

    fn eject(root: &Path, name: &str, workflow: &str) -> Result<PathBuf> {
        TaskCatalog::load(Some(root)).unwrap().eject(
            name,
            workflow,
            &MachineConfig::default(),
            &LoadedDefinitions::default(),
        )
    }

    fn refusal(result: Result<PathBuf>) -> String {
        format!("{:#}", result.expect_err("eject refuses"))
    }

    #[test]
    fn eject_refuses_an_existing_directory_a_named_workflow_task_and_a_room_instance_row() {
        let text = "[tasks.nightly]\nagent = 'claude'\nprompt = 'fix it'\nevery = '1h'\n\
                    [tasks.tidy]\nworkflow = 'babysit'\nevery = '1h'\n\
                    [tasks.night]\nagent = 'claude'\nprompt = 'fix it'\nevery = '1h'\n\
                    fresh-worktree = 'night'\n";
        let root = project(text, true);
        let taken = format!("taken-{}", uuid::Uuid::now_v7().simple());
        let dir = workflows_dir().join(&taken);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workflow.toml"), "[workflow]\n").unwrap();
        assert_eq!(
            refusal(eject(root.path(), "nightly", &taken)),
            format!(
                "workflow `{taken}` already exists at {}; choose another name with \
                 `rimz loop eject nightly <workflow-name>`",
                dir.display()
            )
        );
        assert_eq!(
            refusal(eject(root.path(), "tidy", "tidy")),
            "loop task `tidy` already runs workflow `babysit`; edit that workflow instead"
        );
        let night = format!("night-{}", uuid::Uuid::now_v7().simple());
        assert_eq!(
            refusal(eject(root.path(), "night", &night)),
            "loop task `night` sets `fresh-worktree`, which a workflow task's row refuses; a \
             workflow's steps place their own agents, so remove `fresh-worktree` or keep the \
             task inline"
        );
        assert!(!workflows_dir().join(&night).exists());
        let state = StatePaths::for_project_root(root.path()).unwrap();
        let once = TaskEntry {
            root: root.path().to_owned(),
            agent: Some("claude".into()),
            prompt: Some("once".into()),
            ..TaskEntry::default()
        };
        let instances = super::super::instances::path(&state.root);
        std::fs::create_dir_all(instances.parent().unwrap()).unwrap();
        let rows = crate::config::Tasks([("once".to_owned(), once)].into());
        std::fs::write(instances, serde_json::to_vec(&rows).unwrap()).unwrap();
        assert_eq!(
            refusal(eject(root.path(), "once", "once")),
            "loop task `once` is a room-instance row (a wait or one-shot bound to this room's \
             session); it cannot outlive that session, so it has no workflow to eject"
        );
        assert_eq!(
            refusal(eject(root.path(), "missing", "missing")),
            "no loop task named `missing`; see `rimz loop list`"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join(".rimz/config.toml")).unwrap(),
            text,
            "a refusal leaves every row as it was"
        );

        let untrusted = project("[tasks.shared]\ncheck = 'true'\nevery = '1h'\n", false);
        let shared = format!("shared-{}", uuid::Uuid::now_v7().simple());
        assert_eq!(
            refusal(eject(untrusted.path(), "shared", &shared)),
            format!(
                "loop task `shared` belongs to a project that is not trusted; review it with \
                 `rimz trust` and run `rimz trust grant` in {} first",
                untrusted.path().display()
            )
        );
        assert!(!workflows_dir().join(&shared).exists());
    }

    #[test]
    fn an_ejected_row_launches_once_per_checkout_exactly_when_the_task_did() {
        for (stay, when, dedupes) in [
            (true, None, true),
            (false, Some(()), false),
            (true, Some(()), true),
        ] {
            let entry = TaskEntry {
                agent: Some("claude".into()),
                prompt: Some("settle".into()),
                stay,
                every: when.is_none().then(|| "1h".into()),
                when: when.map(|()| vec!["ci=failed".into()]),
                ..TaskEntry::default()
            };
            assert_eq!(entry.dedupes_checkout(), dedupes, "{entry:?}");
            let ejected = ejected_row(&entry, "settle");
            assert_eq!(
                (ejected.dedupes_checkout(), ejected.once_per_checkout),
                (dedupes, Some(dedupes)),
                "{entry:?}"
            );
        }
    }
}
