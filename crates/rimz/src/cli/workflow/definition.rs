use std::path::Path;

use rimz::config::LaunchKind;
use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::harness::workflow::ScopeKind;

use super::*;

/// A definition `show` and `validate` read: a deployed workflow by name, a directory by path, or
/// a loop task's one-node command graph materialized into a scratch directory named for the task.
pub(super) struct Definition {
    name: String,
    raw: String,
    dir: PathBuf,
    task: Option<Scratch>,
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `raw` is one plain path component, which names a workflow rather than a path.
pub(super) fn one_name(raw: &str) -> bool {
    matches!(
        Path::new(raw).components().collect::<Vec<_>>()[..],
        [std::path::Component::Normal(_)]
    )
}

impl Definition {
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) const fn is_task(&self) -> bool {
        self.task.is_some()
    }

    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the target named the directory by path rather than a deployed name.
    pub(super) fn is_path(&self) -> bool {
        !one_name(&self.raw)
    }

    /// Resolve `raw`, a bare name or one with a `workflow:` or `task:` selector, against the
    /// deployed workflows and the caller's loop tasks, or a path to a directory holding a
    /// definition, named as a run of it is; `None` for any other path or a run target.
    pub(super) fn resolve(raw: &str, globals: &super::super::GlobalFlags) -> Result<Option<Self>> {
        let (selector, name) = match raw.split_once(':') {
            Some((selector @ ("workflow" | "task"), name)) => (Some(selector), name),
            _ => (None, raw),
        };
        if !one_name(name) {
            let dir = PathBuf::from(raw);
            if selector.is_some() || !paths::workflow_spec_file(&dir).is_file() {
                return Ok(None);
            }
            return Ok(Some(Self {
                name: rimz::harness::workflow::directory_name(&dir)
                    .unwrap_or_else(|| raw.to_owned()),
                raw: raw.into(),
                dir,
                task: None,
            }));
        }
        if !resolve::names_workflow(name) {
            return Ok(None);
        }
        let found = if selector == Some("task") {
            None
        } else {
            workflow_view::definition_dir(Path::new(name))?
        };
        let deployed = found.is_some();
        // A name nothing claims stays a path into the home, where reading it fails.
        let dir =
            found.unwrap_or_else(|| paths::workflows_dir_under(&paths::agents_home()).join(name));
        let catalog = (selector != Some("workflow"))
            .then(|| TaskCatalog::load_lenient(validation_root(globals).as_deref()));
        let task = catalog
            .as_ref()
            .and_then(|catalog| catalog.visible().get(name))
            .and_then(|task| match task.action() {
                Ok(rimz::harness::schedule::TaskAction::Shell(argv)) => Some((task, argv)),
                _ => None,
            });
        let (task, argv) = match (deployed, task) {
            (true, Some(_)) if selector.is_none() => {
                return Err(super::super::usage::UsageError::new(format!(
                    "{name} names a deployed workflow and a loop task; use workflow:{name} or task:{name}"
                ))
                .into());
            }
            (_, Some(task)) if !deployed => task,
            _ if selector == Some("task") => bail!("no loop task {name} runs a command"),
            _ => {
                return Ok(Some(Self {
                    name: name.into(),
                    raw: raw.into(),
                    dir,
                    task: None,
                }));
            }
        };
        let definition =
            rimz::harness::schedule::shell_task::command_definition(name, task.entry(), argv)?;
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "rimz-task-{}-{}",
            std::process::id(),
            rimz::ids::WorkflowRunId::new()
        )));
        let dir = scratch.0.join(name);
        for (asset, body) in &definition.assets {
            let path = dir.join(asset);
            std::fs::create_dir_all(path.parent().unwrap_or(&dir))?;
            std::fs::write(&path, body)?;
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(paths::workflow_spec_file(&dir), &definition.text)?;
        Ok(Some(Self {
            name: name.into(),
            raw: raw.into(),
            dir,
            task: Some(scratch),
        }))
    }
}

pub(super) fn render(definition: &Definition, globals: &super::super::GlobalFlags) -> Result<()> {
    let name = definition.name.as_str();
    let mut out = render::out();
    let spec = match load(definition) {
        Ok(spec) => spec,
        Err(error) => {
            writeln!(out, "{error}")?;
            return Ok(());
        }
    };
    let title = spec
        .summary()
        .map_or_else(|| name.to_owned(), |summary| format!("{name} · {summary}"));
    writeln!(out, "{}", render::paint(render::palette::header(), &title))?;
    header(&mut out, definition, &spec, globals)?;
    writeln!(out)?;
    rail::Rail::definition(&spec).write(&mut out, render::terminal_columns(100))?;
    writeln!(out)?;
    for sentence in rail::defaults(&spec) {
        writeln!(out, "{}", render::paint(render::palette::muted(), sentence))?;
    }
    let command = resolve::command(
        "show",
        &[&definition.raw, spec.start()],
        globals.root.as_deref(),
        None,
        &[],
    );
    if definition.task.is_some() {
        writeln!(
            out,
            "one node in full: {command} · compiled from loop task {name}"
        )?;
    } else {
        writeln!(
            out,
            "one node in full: {command} · source {}",
            render::home_relative_path(&paths::workflow_spec_file(&definition.dir))
        )?;
    }
    writeln!(out)?;
    Ok(())
}

pub(super) fn load(definition: &Definition) -> Result<workflow::Definition> {
    workflow::load(&definition.dir).map_err(|error| {
        let unavailable = format!(
            "{}: definition unavailable: {}",
            definition.name,
            render::one_line(&error.to_string())
        );
        if is_missing(&error) {
            return anyhow::anyhow!(unavailable);
        }
        anyhow::anyhow!(
            "{unavailable}; check with {}",
            resolve::command("validate", &[&definition.raw], None, None, &[]),
        )
    })
}

/// A definition whose file is gone has nothing `rimz workflow validate` could check.
fn is_missing(error: &workflow::SpecErr) -> bool {
    let workflow::SpecErr::Io { source, .. } = error else {
        return false;
    };
    source.kind() == std::io::ErrorKind::NotFound
}

fn header(
    out: &mut impl Write,
    definition: &Definition,
    spec: &workflow::Definition,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let name = definition.name.as_str();
    let scope = match spec.scope() {
        ScopeKind::Worktree => "worktree",
        ScopeKind::Project => "project",
    };
    let root = validation_root(globals);
    let catalog = TaskCatalog::load_lenient(root.as_deref());
    let tasks = catalog
        .visible()
        .iter()
        .filter_map(|(task_name, task)| {
            let starts = match (&definition.task, task.action()) {
                (Some(_), _) => task_name == name,
                (None, Ok(rimz::harness::schedule::TaskAction::Launch(launch))) => {
                    !definition.is_path()
                        && launch.kind == LaunchKind::Workflow
                        && launch.target == name
                }
                (None, _) => false,
            };
            if !starts {
                return None;
            }
            Some(format!(
                "started by loop task {task_name}: {}",
                task.trigger().as_ref().ok()?.describe()
            ))
        })
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        let worktree = spec.scope() == ScopeKind::Worktree;
        let flags: &[&str] = if worktree { &["-w", "<worktree>"] } else { &[] };
        let root = globals.root.as_deref();
        let target = if definition.is_path() {
            &definition.raw
        } else {
            name
        };
        let command = resolve::command("run", &[target], root, None, flags);
        let rimz = resolve::rimz_prefix(root);
        let task = format!(
            "{rimz} loop add '<task>' --workflow {}{} --cron '<expression>'",
            crate::cli::usage::shell_command([name]),
            if worktree { " --each-worktree" } else { "" },
        );
        // A loop task takes only a deployed name, so a directory by path is run by hand.
        if definition.is_path() {
            writeln!(out, "one run per {scope} · by hand: {command}")?;
        } else {
            writeln!(
                out,
                "one run per {scope} · no loop task starts it; from a loop: {task} · by hand: \
                 {command}"
            )?;
        }
    } else {
        writeln!(out, "one run per {scope} · {}", tasks.join(" · "))?;
    }
    if let Some(limit) = spec.limit() {
        writeln!(out, "limit {}", limit_text(limit))?;
    }
    if let Some(deadline) = spec.deadline() {
        writeln!(
            out,
            "deadline {deadline} fails the node the run is at (a live attempt is canceled)"
        )?;
    }
    if let Some(target) = spec.on_failed() {
        writeln!(out, "on.failed → {target}")?;
    }
    Ok(())
}
