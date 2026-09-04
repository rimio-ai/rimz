use std::io::Write;

use anyhow::{Result, bail};

use rimz::harness::schedule::catalog::{TaskCatalog, TaskSource};
use rimz::harness::schedule::signal::stop_watcher;

use super::*;

pub(super) fn run(name: &str, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let caller = caller(&ctx)?;
    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    let Some(task) = catalog.visible().get(name) else {
        bail!("no pending wake named `{name}`; see `rimz wake list`");
    };
    let target = task.entry().wake.as_ref();
    let belongs_here = task.source() == TaskSource::Instance
        && task.entry().resolved_root() == ctx.workspace.project_root
        && target.is_some();
    let belongs_to_caller = caller
        .as_ref()
        .and_then(|caller| caller.launch_id.as_ref())
        .is_none_or(|session| target.is_some_and(|target| target.session == session.as_str()));
    if !belongs_here || !belongs_to_caller {
        bail!("no pending wake named `{name}`; see `rimz wake list`");
    }
    catalog.remove(name)?;
    stop_watcher(ctx.runtime(), name)?;
    writeln!(super::super::render::out(), "canceled {name}")?;
    Ok(())
}
