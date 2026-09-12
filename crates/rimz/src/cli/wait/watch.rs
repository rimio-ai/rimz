use super::*;

pub(super) fn run(name: &str, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    rimz::harness::schedule::signal::run_watcher(&ctx.store, &ctx.workspace, name)
}
