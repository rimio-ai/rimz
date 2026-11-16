//! Comment-preserving TOML task stores for machine `loop.toml` and project `.rimz/config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Table};

use crate::config::{MachineConfig, TaskEntry, remove_keeping_comments, set_item_keeping_comments};
use crate::disk::atomic::write_bytes_atomically;
use crate::disk::paths::{agents_home, holds_rimz_home, rimz_home};
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
                if holds_rimz_home(project_root, &rimz_home()) {
                    bail!(
                        "{} holds the RimZ home, so its config.toml is the machine config; \
                         run from a project directory, or drop --project to edit machine tasks",
                        project_root.display()
                    );
                }
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
    let text = store.read_text()?;
    if matches!(store, TaskStore::Machine) {
        let config: crate::config::LoopConfig =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if let Some(existing) = config.tasks.0.get(name)
            && existing.resolved_root() != entry.resolved_root()
        {
            if let Some(problem) = existing.root_problem() {
                bail!(
                    "loop task `{name}` has {problem} in {}; choose another name, or run \
                     `rimz loop remove {name}` from a fresh directory outside any repository \
                     or RimZ project to remove it first",
                    path.display()
                );
            }
            bail!(
                "loop task `{name}` belongs to another project at {}; choose another name, \
                 or run `rimz loop remove {name}` from that project first",
                existing.root.display()
            );
        }
    }
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let item = Item::Table(task_entry_table(
        entry,
        matches!(store, TaskStore::Machine),
    )?);
    root_tasks_table(&mut doc)?;
    set_item_keeping_comments(&mut doc, &["tasks".to_owned(), name.to_owned()], item)?;

    let rendered = doc.to_string();
    store.validate(&rendered, name)?;
    write_bytes_atomically(&path, rendered.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub(super) fn remove(store: TaskStore<'_>, name: &str) -> Result<bool> {
    remove_at_root(store, name, None)
}

pub(super) fn remove_machine_at_root(name: &str, root: &Path) -> Result<bool> {
    remove_at_root(TaskStore::Machine, name, Some(root))
}

fn remove_at_root(store: TaskStore<'_>, name: &str, root: Option<&Path>) -> Result<bool> {
    let path = store.path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    if let Some(root) = root {
        let config: crate::config::LoopConfig =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if !config
            .tasks
            .0
            .get(name)
            .is_some_and(|entry| entry.resolved_root() == root)
        {
            return Ok(false);
        }
    }
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    let removed = remove_keeping_comments(&mut doc, &["tasks".to_owned(), name.to_owned()])?;
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
    serializable.dir = entry
        .dir
        .as_deref()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()));
    let mut table = toml_edit::ser::to_document(&serializable)
        .context("serializing loop task")?
        .into_table();
    if !include_root {
        table.remove("root");
        table.remove("dir");
    }
    if let Some(wait) = table.remove("wait") {
        table.insert(
            "wait",
            Item::Table(
                wait.into_table()
                    .map_err(|_| anyhow::anyhow!("serialized loop wait is not a table"))?,
            ),
        );
    }
    if let Some(matches) = table.remove("match") {
        table.insert(
            "match",
            Item::Table(
                matches
                    .into_table()
                    .map_err(|_| anyhow::anyhow!("serialized loop match is not a table"))?,
            ),
        );
    }
    Ok(table)
}

fn root_tasks_table(doc: &mut DocumentMut) -> Result<&mut Table> {
    let tasks = doc
        .as_table_mut()
        .entry("tasks")
        .or_insert_with(|| {
            let mut tasks = Table::new();
            tasks.set_implicit(true);
            Item::Table(tasks)
        })
        .as_table_mut()
        .context("`tasks` is not a table")?;
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_fields_round_trip_machine_and_instance_rows() {
        let source = r#"
agent = "claude,codex"
root = "/repo"
stay = true
each-worktree = true
takeover = true
subscribe = [{ signal = "ci.failed", match = { branch = "feature" }, prompt = "repair" }]
"#;
        let entry: TaskEntry = toml::from_str(source).unwrap();
        let table = task_entry_table(&entry, true).unwrap();
        for field in ["stay", "each-worktree", "takeover", "subscribe"] {
            assert!(table.contains_key(field), "machine row lost {field}");
        }
        let decoded: TaskEntry = toml_edit::de::from_document(DocumentMut::from(table)).unwrap();
        assert_eq!(decoded, entry);
        let instances = crate::config::Tasks(std::collections::BTreeMap::from([(
            "resident".to_owned(),
            entry,
        )]));
        let json = serde_json::to_value(&instances).unwrap();
        assert_eq!(json["resident"]["stay"], true);
        assert_eq!(
            json["resident"]["subscribe"][0]["match"]["branch"],
            "feature"
        );
        assert_eq!(
            serde_json::from_value::<crate::config::Tasks>(json).unwrap(),
            instances
        );
    }
    use crate::config::CheckOn;

    /// The machine row's text and the room's instance file, each written, read back, and
    /// written again.
    fn rewritten(entry: &TaskEntry) -> [(String, String); 2] {
        let machine = |entry: &TaskEntry| {
            DocumentMut::from(task_entry_table(entry, true).unwrap()).to_string()
        };
        let first = machine(entry);
        let read: TaskEntry = toml::from_str(&first).unwrap();
        let machine = (first, machine(&read));
        let dir = tempfile::tempdir().unwrap();
        let state = crate::disk::paths::StatePaths::for_project_root_under(
            &entry.root,
            &dir.path().join("home"),
        )
        .unwrap();
        let workspace = crate::workspace::WorkspaceResolver::resolve(&entry.root, None).unwrap();
        crate::workspace::record::write(
            &state,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        let instance = || {
            let bytes = std::fs::read(super::super::instances::path(&state.root)).unwrap();
            String::from_utf8(bytes).unwrap()
        };
        super::super::instances::insert(&state, "booking", entry).unwrap();
        let first = instance();
        let read = super::super::instances::load_strict_from(&state.root).unwrap();
        super::super::instances::insert(&state, "booking", &read.0["booking"]).unwrap();
        [machine, (first, instance())]
    }

    #[test]
    fn booking_chains_round_trip_and_old_rows_rewrite_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let old = TaskEntry {
            args: vec!["rimz".to_owned(), "teams".to_owned(), "forge".to_owned()],
            timeout: Some("2h".to_owned()),
            when: Some(vec!["pr[x]=merged".to_owned()]),
            root: dir.path().to_path_buf(),
            ..TaskEntry::default()
        };
        for (first, again) in rewritten(&old) {
            assert_eq!(first, again);
            assert!(!first.contains("booked"), "{first}");
        }
        let booked = TaskEntry {
            booked_by: ["launch_lead", "launch_user_shell"]
                .map(|id| crate::agents::LaunchedBy {
                    kind: crate::ids::AgentKind::new_unchecked("claude"),
                    agent_id: crate::ids::AgentSessionId::from(id),
                })
                .to_vec(),
            ..old
        };
        for (first, again) in rewritten(&booked) {
            assert_eq!(first, again);
            assert!(
                first.contains("booked-by") && first.contains("launch_user_shell"),
                "{first}"
            );
        }
    }

    #[test]
    fn task_entry_serialization_uses_canonical_schema_and_store_policy() {
        let entry = TaskEntry {
            label: Some("nightly".to_owned()),
            kind: Some(crate::config::LaunchKind::Shell),
            target: None,
            args: vec!["cargo".to_owned(), "test".to_owned()],
            each_worktree: false,
            team: None,
            loop_task: None,
            wait: None,
            wait_meta: None,
            prompt: None,
            max_strikes: Some(4),
            once_per_checkout: Some(false),
            on: Some(CheckOn::Success),
            root: PathBuf::from("/repo"),
            dir: Some(PathBuf::from("/linked")),
            fresh_worktree: Some("fire".to_owned()),
            timeout: Some("20m".to_owned()),
            at: Some("07:00".to_owned()),
            every: Some("1h".to_owned()),
            cron: Some("0 * * * *".to_owned()),
            signal: Some("ci.failed".to_owned()),
            when: Some(vec!["team.stage=Done".to_owned(), "ci=passed".to_owned()]),
            hold: Some("30m".to_owned()),
            matches: Some(std::collections::BTreeMap::from([(
                "branch".to_owned(),
                "feature".to_owned(),
            )])),
            watch: None,
            repeat: true,
            deadline: Some(jiff::Timestamp::UNIX_EPOCH),
            fire_at: Some(jiff::Timestamp::UNIX_EPOCH),
            provider: Some(crate::ids::AgentKind::new_unchecked("claude")),
            account: Some("work".parse().expect("login name")),
            overlap: None,
            overlap_limit: None,
            booked_by: Vec::new(),
            refused: None,
        };

        let machine = task_entry_table(&entry, true).expect("serialize machine");
        let mut doc = DocumentMut::new();
        root_tasks_table(&mut doc)
            .expect("tasks table")
            .insert("full", Item::Table(machine.clone()));
        let machine_text = doc.to_string();
        assert!(machine.contains_key("root"));
        assert!(machine.contains_key("dir"));
        assert!(machine_text.contains("fresh-worktree = \"fire\""));
        assert!(machine.contains_key("max-strikes"));
        assert!(machine_text.contains("args = [\"cargo\", \"test\"]"));
        assert!(machine_text.contains("repeat = true"));
        assert!(machine.contains_key("fire-at"));
        assert!(machine_text.contains("[tasks.full.match]"));
        assert!(machine_text.contains("branch = \"feature\""));
        assert!(machine_text.contains("when = [\"team.stage=Done\", \"ci=passed\"]"));
        assert!(machine_text.contains("for = \"30m\""));
        assert!(machine_text.contains("provider = \"claude\""));
        assert!(machine_text.contains("account = \"work\""));
        assert_eq!(
            toml_edit::de::from_document::<TaskEntry>(DocumentMut::from(machine))
                .expect("machine round trip"),
            entry
        );

        let project = task_entry_table(&entry, false).expect("serialize project");
        assert!(!project.contains_key("root"));
        assert!(!project.contains_key("dir"));
        assert_eq!(project["fresh-worktree"].as_str(), Some("fire"));
        let mut project_round =
            toml_edit::de::from_document::<TaskEntry>(DocumentMut::from(project))
                .expect("project round trip");
        project_round.root = entry.root.clone();
        project_round.dir = entry.dir.clone();
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
            dir: Some(path.clone()),
            ..TaskEntry::default()
        };

        let table = task_entry_table(&entry, true).expect("serialize lossy paths");
        let expected = path.to_string_lossy();
        assert_eq!(table["root"].as_str(), Some(expected.as_ref()));
        assert_eq!(table["dir"].as_str(), Some(expected.as_ref()));
    }

    /// The machine, project, instance, and edit readers of one stored row read it alike, a
    /// refused row included.
    #[test]
    fn stored_rows_compile_alike_through_all_four_readers() {
        let shared = [
            "args = ['cargo', 'test']\ntimeout = '1h'\nevery = '1h'\noverlap = 'queue'",
            "args = ['true']\ntimeout = '5m'\nsignal = 'ci.failed'\nrepeat = true",
            "agent = 'claude'\nprompt = 'fix'\nevery = '1h'",
        ];
        let machine_only = [
            "args = ['true']\ntimeout = '5m'\neach-worktree = true\nevery = '1h'",
            "args = ['true']\ntimeout = '5m'\nevery = '10m'\ndeadline = '2030-01-01T00:00:00Z'",
            "args = ['true']\ntimeout = '5m'\nwhen = ['ci=passed']\nfor = '5m'",
            "once = true\nwait = { kind = 'claude', session = 's', handle = '@c' }\nat = '07:00'",
        ];
        let dir = tempfile::tempdir().unwrap();
        let compiled = |entry: &TaskEntry| {
            let shape = super::super::TaskShape::compile("task", entry);
            let trigger = shape
                .trigger()
                .as_ref()
                .map(super::super::ParsedTrigger::describe);
            (
                shape.action().cloned().map_err(ToString::to_string),
                trigger.map_err(ToString::to_string),
            )
        };
        for (row, project) in shared
            .iter()
            .map(|row| (row, true))
            .chain(machine_only.iter().map(|row| (row, false)))
        {
            let machine_text = format!("[tasks.task]\nroot = '/repo'\n{row}\n");
            let machine =
                MachineConfig::parse_text(Path::new("loop.toml"), &machine_text, dir.path())
                    .unwrap()
                    .r#loop
                    .tasks
                    .0
                    .remove("task")
                    .unwrap();
            let expected = compiled(&machine);
            let refused = row.contains("agent") || row.contains("once");
            assert_eq!(expected.0.is_err(), refused, "{row}: {expected:?}");

            if project {
                let value = toml::from_str(&format!("[tasks.task]\n{row}\n")).unwrap();
                let project = crate::config::effective::project_tasks_from_value(
                    Path::new("/repo"),
                    Path::new("/repo/.rimz/config.toml"),
                    TrustState::Trusted,
                    &value,
                )
                .unwrap()
                .unwrap()
                .tasks
                .0
                .remove("task")
                .unwrap();
                assert_eq!(compiled(&project), expected, "project reader: {row}");
            }

            let instances = crate::config::Tasks([("task".to_owned(), machine.clone())].into());
            let instances_path = super::super::instances::path(dir.path());
            std::fs::create_dir_all(instances_path.parent().unwrap()).unwrap();
            std::fs::write(instances_path, serde_json::to_vec(&instances).unwrap()).unwrap();
            let mut loaded = super::super::instances::load_strict_from(dir.path()).unwrap();
            let instance = loaded.0.remove("task").unwrap();
            assert_eq!(compiled(&instance), expected, "instance reader: {row}");

            let mut doc = DocumentMut::new();
            root_tasks_table(&mut doc).unwrap().insert(
                "task",
                Item::Table(task_entry_table(&machine, true).unwrap()),
            );
            let edited =
                MachineConfig::parse_text(Path::new("loop.toml"), &doc.to_string(), dir.path())
                    .unwrap()
                    .r#loop
                    .tasks
                    .0
                    .remove("task")
                    .unwrap();
            assert_eq!(edited, machine, "edit round trip: {row}");
            assert_eq!(compiled(&edited), expected, "edit reader: {row}");
        }
    }
}
