use anyhow::Result;
use serde::Serialize;

use rimz::harness::schedule::Trigger;
use rimz::harness::schedule::catalog::{LoadedTask, TaskCatalog, TaskSource};
use rimz::harness::schedule::signal::watcher_info;

use super::*;

#[derive(Serialize)]
struct WakeRow {
    name: String,
    trigger: String,
    target: String,
    age: String,
    state: String,
}

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let caller_session = caller_session(&ctx)?;
    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    let rows = catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.source() == TaskSource::Instance)
        .filter(|(_, task)| task.entry().resolved_root() == ctx.workspace.project_root)
        .filter(|(_, task)| task.entry().wake.is_some())
        .filter(|(_, task)| {
            caller_session.as_ref().is_none_or(|session| {
                task.entry()
                    .wake
                    .as_ref()
                    .is_some_and(|target| target.session == session.as_str())
            })
        })
        .map(|(name, task)| row(&ctx, name, task))
        .collect::<Result<Vec<_>>>()?;
    if json {
        return super::super::render::json(&rows);
    }
    let mut table = super::super::render::Table::new(["NAME", "STATE", "TARGET", "AGE", "TRIGGER"])
        .max_width(super::super::render::terminal_columns(120));
    for row in rows {
        table.row([
            super::super::render::cell(row.name),
            super::super::render::cell(row.state),
            super::super::render::cell(row.target),
            super::super::render::cell(row.age),
            super::super::render::cell(row.trigger),
        ]);
    }
    table.render(&mut super::super::render::out())?;
    Ok(())
}

fn row(ctx: &Ctx, name: &str, task: &LoadedTask) -> Result<WakeRow> {
    let parsed = task.trigger().as_ref().map_err(Clone::clone)?;
    let target = task
        .entry()
        .wake
        .as_ref()
        .expect("wake rows have delivery targets");
    let (age, state) = match &parsed.trigger {
        Trigger::Schedule(_) => (
            "-".to_owned(),
            format!("due {}", task.entry().at.as_deref().unwrap_or("now")),
        ),
        Trigger::Signal { .. } => {
            let now = jiff::Timestamp::now();
            let state = task.entry().deadline.map_or_else(
                || "waiting".to_owned(),
                |deadline| {
                    format!(
                        "waiting · {} left",
                        super::super::render::age_short(now, deadline.max(now))
                    )
                },
            );
            let age = task.entry().wake_meta.as_ref().map_or_else(
                || "-".to_owned(),
                |meta| super::super::render::age_short(meta.armed_at, now),
            );
            (age, state)
        }
        Trigger::Watch { .. } => match watcher_info(ctx.runtime(), name)? {
            Some(info) => (
                super::super::render::age_short(info.started_at, jiff::Timestamp::now()),
                format!("watching pid {}", info.pid),
            ),
            None => ("-".to_owned(), "watcher lost".to_owned()),
        },
    };
    Ok(WakeRow {
        name: name.to_owned(),
        trigger: parsed.describe(),
        target: target.handle.clone(),
        age,
        state,
    })
}
