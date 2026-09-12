use anyhow::Result;
use serde::Serialize;
use std::io::Write;

use rimz::harness::schedule::Trigger;
use rimz::harness::schedule::arming::{self, ArmState};
use rimz::harness::schedule::catalog::{LoadedTask, TaskCatalog};
use rimz::harness::schedule::pending::session_deliveries;
use rimz::harness::schedule::signal::watcher_info;

use super::*;

#[derive(Serialize)]
pub(super) struct WakeRow {
    pub(super) name: String,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<String>,
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
    let arming = arming::load();
    let now = jiff::Timestamp::now();
    session_deliveries(
        &catalog,
        &ctx.workspace.project_root,
        caller_session.as_ref(),
    )
    .map(|(name, task)| {
        let state = ArmState::resolve(arming.get(&task.key(name)), task.source(), now);
        row(ctx, name, task, state)
    })
    .collect()
}

pub(super) fn write_rows(out: &mut impl Write, rows: Vec<WakeRow>) -> Result<()> {
    if rows.is_empty() {
        writeln!(out, "no pending waits")?;
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

fn row(ctx: &Ctx, name: &str, task: &LoadedTask, arm_state: ArmState) -> Result<WakeRow> {
    let parsed = task.trigger().as_ref().map_err(Clone::clone)?;
    let target = task
        .entry()
        .wait
        .as_ref()
        .expect("wait rows have delivery targets");
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
            let age = task.entry().wait_meta.as_ref().map_or_else(
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
    let dir = task
        .entry()
        .dir
        .as_deref()
        .map(|dir| super::super::render::home_relative(&dir.to_string_lossy()));
    Ok(WakeRow {
        name: name.to_owned(),
        trigger: match &parsed.trigger {
            Trigger::Watch { command } => {
                let trigger = task
                    .entry()
                    .wait_meta
                    .as_ref()
                    .and_then(|meta| meta.pid)
                    .map_or_else(
                        || format!("watch: {}", rimz::theme::fmt::command_preview(command)),
                        |pid| format!("pid {pid}"),
                    );
                match dir.as_deref() {
                    Some(dir) => format!("{trigger} · in {dir}"),
                    None => trigger,
                }
            }
            _ => parsed.describe(),
        },
        dir,
        target: target.handle.clone(),
        age,
        state: match arm_state {
            ArmState::Live => state,
            ArmState::Disabled(_) => "disabled".to_owned(),
            ArmState::Paused(until) => format!(
                "paused · {}",
                super::super::render::rel_until(until, jiff::Timestamp::now())
            ),
        },
    })
}
