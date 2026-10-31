use std::path::Path;

use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::harness::workflow::{EdgeSource, ScopeKind, WorkflowSpec};

use super::*;

pub(super) fn render(name: &str, globals: &super::super::GlobalFlags) -> Result<()> {
    let dir = definition_dir(Path::new(name));
    let mut out = render::out();
    let spec = match load(name) {
        Ok(spec) => spec,
        Err(error) => {
            writeln!(out, "{error}")?;
            return Ok(());
        }
    };
    let title = spec
        .workflow
        .summary
        .as_ref()
        .map_or_else(|| name.to_owned(), |summary| format!("{name} · {summary}"));
    writeln!(out, "{}", render::paint(render::palette::header(), &title))?;
    header(&mut out, name, &spec, globals)?;
    writeln!(out)?;
    rail::Rail::definition(&spec).write(&mut out, render::terminal_columns(100))?;
    writeln!(out)?;
    for sentence in rail::defaults(&spec) {
        writeln!(out, "{}", render::paint(render::palette::muted(), sentence))?;
    }
    let command = resolve::command(
        "show",
        &[name, &spec.workflow.start],
        globals.root.as_deref(),
        None,
        &[],
        &[],
    );
    writeln!(
        out,
        "one node in full: {command} · source {}",
        render::home_relative_path(&paths::workflow_spec_file(&dir))
    )?;
    writeln!(out)?;
    Ok(())
}

pub(super) fn load(name: &str) -> Result<WorkflowSpec> {
    workflow::load(&definition_dir(Path::new(name))).map_err(|error| {
        anyhow::anyhow!(
            "{name}: definition unavailable: {}; check with {}",
            render::one_line(&error.to_string()),
            resolve::command("validate", &[name], None, None, &[], &[]),
        )
    })
}

fn header(
    out: &mut impl Write,
    name: &str,
    spec: &WorkflowSpec,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let scope = match spec.workflow.scope {
        ScopeKind::Worktree => "worktree",
        ScopeKind::Project => "project",
    };
    let root = validation_root(globals);
    let catalog = TaskCatalog::load_lenient(root.as_deref());
    let tasks = catalog
        .visible()
        .iter()
        .filter_map(|(task_name, task)| {
            if task.entry().workflow.as_deref() != Some(name) {
                return None;
            }
            Some(format!(
                "started by loop task {task_name}: {}",
                task.trigger().as_ref().ok()?.describe()
            ))
        })
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        let flags: &[&str] = if spec.workflow.scope == ScopeKind::Worktree {
            &["-w", "<worktree>"]
        } else {
            &[]
        };
        let command = resolve::command("run", &[name], globals.root.as_deref(), None, &[], flags);
        writeln!(
            out,
            "one run per {scope} · no loop task starts it; by hand: {command}"
        )?;
    } else {
        writeln!(out, "one run per {scope} · {}", tasks.join(" · "))?;
    }
    let edge = spec
        .edges(&spec.workflow.start)
        .into_iter()
        .find(|edge| edge.outcome == "deadline");
    if let Some(edge) = edge {
        if let Some(deadline) = &spec.workflow.deadline {
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
