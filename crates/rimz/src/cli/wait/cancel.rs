use std::io::Write;

use anyhow::{Result, bail};

use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::harness::schedule::signal::stop_watcher;

use super::*;

pub(super) fn run(
    name: Option<TaskName>,
    all: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    caller_session(&ctx)?.context(
        "canceling a wait requires an agent RimZ can identify; run this command from an agent pane",
    )?;
    let pending = list::pending_rows(&ctx)?;
    let names = if all {
        pending.into_iter().map(|row| row.name).collect::<Vec<_>>()
    } else {
        let name = name.expect("clap requires a name or --all").to_string();
        if !pending.iter().any(|row| row.name == name) {
            bail!("no pending wait named `{name}`; see `rimz wait list`");
        }
        vec![name]
    };
    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    for name in &names {
        catalog.remove(name)?;
        stop_watcher(ctx.runtime(), name)?;
    }
    let pending = list::pending_rows(&ctx)?;
    if json {
        return super::super::render::json(
            &serde_json::json!({ "canceled": names, "pending": pending }),
        );
    }
    let mut out = super::super::render::out();
    if !names.is_empty() {
        writeln!(out, "canceled {}", names.join(", "))?;
    }
    list::write_rows(&mut out, pending)
}
