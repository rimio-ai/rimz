use anyhow::Result;
use serde::Serialize;
use std::io::Write;

use rimz::harness::schedule::Trigger;
use rimz::harness::schedule::catalog::{LoadedTask, TaskCatalog, TaskSource};
use rimz::harness::schedule::signal::watcher_info;

use super::*;

#[derive(Serialize)]
pub(super) struct WakeRow {
    pub(super) name: String,
    trigger: String,
    target: String,
    age: String,
    state: String,
}

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let rows = pending_rows(&ctx)?;
    if json {
        return super::super::render::json(&rows);
    }
    write_rows(&mut super::super::render::out(), rows)
}

pub(super) fn pending_rows(ctx: &Ctx) -> Result<Vec<WakeRow>> {
    let caller_session = caller_session(ctx)?;
    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.source() == TaskSource::Instance)
        .filter(|(_, task)| task.entry().resolved_root() == ctx.workspace.project_root)
        .filter(|(_, task)| task.entry().wake.is_some())
        .filter(|(_, task)| {
            caller_session.as_ref().is_none_or(|(kind, session)| {
                task.entry()
                    .wake
                    .as_ref()
                    .is_some_and(|target| target.kind == *kind && target.session == *session)
            })
        })
        .map(|(name, task)| row(ctx, name, task))
        .collect()
}

pub(super) fn write_rows(out: &mut impl Write, rows: Vec<WakeRow>) -> Result<()> {
    if rows.is_empty() {
        writeln!(out, "no pending wakes")?;
        return Ok(());
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
    table.render(out)?;
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
        trigger: match &parsed.trigger {
            Trigger::Watch { command } => {
                format!("watch: {}", rimz::theme::fmt::command_preview(command))
            }
            _ => parsed.describe(),
        },
        target: target.handle.clone(),
        age,
        state,
    })
}
