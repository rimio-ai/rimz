use anyhow::Result;
use serde::Serialize;
use std::io::Write;

use rimz::harness::schedule::Trigger;
use rimz::harness::schedule::arm::duration_label;
use rimz::harness::schedule::arming::{self, ArmState};
use rimz::harness::schedule::catalog::{LoadedTask, TaskCatalog};
use rimz::harness::schedule::pending::session_deliveries;
use rimz::harness::schedule::runner::parse_task_timeout;
use rimz::harness::schedule::signal::watcher_info;

use super::*;

#[derive(Serialize)]
pub(super) struct WakeRow {
    pub(super) name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    state: &'static str,
    target: String,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<String>,
    elapsed: Option<String>,
    elapsed_s: Option<u64>,
    timeout: Option<String>,
    timeout_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    watcher_pid: Option<u32>,
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
        row(ctx, name, task, state, now)
    })
    .collect()
}

pub(super) fn write_rows(out: &mut impl Write, rows: Vec<WakeRow>) -> Result<()> {
    if rows.is_empty() {
        writeln!(out, "no pending waits")?;
        return Ok(());
    }
    let mut table = super::super::render::Table::new([
        "NAME", "TYPE", "STATE", "TARGET", "ELAPSED", "TIMEOUT", "TRIGGER",
    ])
    .max_width(super::super::render::terminal_columns(120));
    for row in rows {
        table.row([
            super::super::render::cell(row.name),
            super::super::render::cell(row.kind),
            super::super::render::cell(row.state),
            super::super::render::cell(row.target),
            super::super::render::cell(row.elapsed.as_deref().unwrap_or("-")),
            super::super::render::cell(row.timeout.as_deref().unwrap_or("-")),
            super::super::render::cell(row.trigger),
        ]);
    }
    table.render(out)?;
    Ok(())
}

fn row(
    ctx: &Ctx,
    name: &str,
    task: &LoadedTask,
    arm_state: ArmState,
    now: jiff::Timestamp,
) -> Result<WakeRow> {
    let parsed = task.trigger().as_ref().map_err(Clone::clone)?;
    let entry = task.entry();
    let target = entry
        .wait
        .as_ref()
        .expect("wait rows have delivery targets");
    let delay = entry
        .wait_meta
        .as_ref()
        .and_then(|meta| meta.delay.as_deref());
    let (kind, state, timeout, watcher_pid) = match &parsed.trigger {
        Trigger::Schedule(_) => match delay {
            Some(delay) => {
                let seconds = parse_task_timeout(delay)
                    .map_err(anyhow::Error::msg)?
                    .as_secs();
                ("timer", "pending", Some((delay.to_owned(), seconds)), None)
            }
            None => ("schedule", "waiting", None, None),
        },
        Trigger::Signal { .. } => ("signal", "waiting", None, None),
        Trigger::Condition { .. } => ("condition", "waiting", None, None),
        Trigger::Watch(spec) => {
            let pid = watcher_info(ctx.runtime(), name)?.map(|info| info.pid);
            // A watch armed under cache keepalive records no check-in limit.
            let timeout = entry
                .timeout
                .as_deref()
                .map(parse_task_timeout)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            (
                spec.kind(),
                if pid.is_some() { "watching" } else { "lost" },
                timeout.map(|timeout| (duration_label(timeout), timeout.as_secs())),
                pid,
            )
        }
    };
    let meta = match &parsed.trigger {
        Trigger::Watch(_) => entry.wait_meta.as_ref(),
        Trigger::Schedule(_) if delay.is_some() => entry.wait_meta.as_ref(),
        Trigger::Schedule(_) | Trigger::Signal { .. } | Trigger::Condition { .. } => None,
    };
    let dir = entry
        .dir
        .as_deref()
        .map(|dir| super::super::render::home_relative(&dir.to_string_lossy()));
    let mut trigger = match &parsed.trigger {
        Trigger::Watch(spec) => {
            let trigger = entry.label.clone().unwrap_or_else(|| spec.subject());
            match dir.as_deref() {
                Some(dir) => format!("{trigger} · in {dir}"),
                None => trigger,
            }
        }
        Trigger::Schedule(_) | Trigger::Signal { .. } | Trigger::Condition { .. } => {
            parsed.describe()
        }
    };
    let state = match arm_state {
        ArmState::Live => state,
        ArmState::Disabled(_) => "disabled",
        ArmState::Paused(until) => {
            trigger.push_str(&format!(
                " · resumes {}",
                super::super::render::rel_until(until, now)
            ));
            "paused"
        }
    };
    Ok(WakeRow {
        name: name.to_owned(),
        kind,
        state,
        target: target.handle.clone(),
        trigger,
        label: entry.label.clone(),
        dir,
        elapsed: meta.map(|meta| super::super::render::age_short(meta.armed_at, now)),
        elapsed_s: meta.map(|meta| now.duration_since(meta.armed_at).as_secs().max(0) as u64),
        timeout_s: timeout.as_ref().map(|(_, seconds)| *seconds),
        timeout: timeout.map(|(label, _)| label),
        watcher_pid,
    })
}
