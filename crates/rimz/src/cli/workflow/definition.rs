use std::path::Path;

use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::harness::schedule::compile::{CompiledAction, TaskWorkflow};
use rimz::harness::workflow::{EdgeSource, ScopeKind};

use super::*;

/// A definition `show` and `validate` read by name: a deployed workflow, or a loop task's
/// compiled graph materialized into a scratch directory named for the task.
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

    /// Resolve `raw`, a bare name or one with a `workflow:` or `task:` selector, against the
    /// deployed workflows and the caller's loop tasks; `None` for a path or a run target.
    pub(super) fn resolve(raw: &str, globals: &super::super::GlobalFlags) -> Result<Option<Self>> {
        let (selector, name) = match raw.split_once(':') {
            Some((selector @ ("workflow" | "task"), name)) => (Some(selector), name),
            _ => (None, raw),
        };
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
            || !resolve::names_workflow(name)
        {
            return Ok(None);
        }
        let dir = workflow_view::definition_dir(Path::new(name));
        let deployed = selector != Some("task") && dir.is_dir();
        let catalog = (selector != Some("workflow"))
            .then(|| TaskCatalog::load_lenient(validation_root(globals).as_deref()));
        let task = catalog
            .as_ref()
            .and_then(|catalog| catalog.visible().get(name))
            .and_then(|task| match task.compiled() {
                Ok(CompiledAction::Workflow(TaskWorkflow::Inline(inline))) => Some((task, inline)),
                _ => None,
            });
        let (task, inline) = match (deployed, task) {
            (true, Some(_)) if selector.is_none() => {
                return Err(super::super::usage::UsageError::new(format!(
                    "{name} names a deployed workflow and a loop task; use workflow:{name} or task:{name}"
                ))
                .into());
            }
            (_, Some(task)) if !deployed => task,
            _ if selector == Some("task") => bail!("no loop task {name} compiles to a workflow"),
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
            inline.definition(name, task, &rimz::config::MachineConfig::load_lenient())?;
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
        anyhow::anyhow!(
            "{}: definition unavailable: {}; check with {}",
            definition.name,
            render::one_line(&error.to_string()),
            resolve::command("validate", &[&definition.raw], None, None, &[], &[]),
        )
    })
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
            let started = if definition.task.is_some() {
                task_name == name
            } else {
                task.entry().workflow.as_deref() == Some(name)
            };
            if !started {
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
        let command = resolve::command("run", &[name], root, None, &[], flags);
        let task = format!(
            "{} loop add '<task>' --workflow={}{} --every '<duration>'",
            resolve::rimz_prefix(root),
            crate::cli::usage::shell_command([name]),
            if worktree { " --each-worktree" } else { "" },
        );
        writeln!(
            out,
            "one run per {scope} · no loop task starts it; from a loop: {task} · by hand: {command}"
        )?;
    } else {
        writeln!(out, "one run per {scope} · {}", tasks.join(" · "))?;
    }
    if let Some(limit) = spec.limit() {
        writeln!(out, "limit {}", limit_text(limit))?;
    }
    let edge = spec
        .edges(spec.start())
        .into_iter()
        .find(|edge| edge.outcome == "deadline");
    if let Some(edge) = edge {
        if let Some(deadline) = spec.deadline() {
            writeln!(
                out,
                "deadline {deadline} → {} (a live attempt is canceled)",
                edge.target
            )?;
        } else if edge.source == EdgeSource::Explicit {
            writeln!(
                out,
                "when a deadline granted to a run passes → {} (a live attempt is canceled)",
                edge.target
            )?;
        }
    }
    Ok(())
}
