use super::*;

pub(super) fn run(name: &str, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let paths = ctx.store.paths();
    rimz::disk::capped_log::watch_own_stderr(paths, &paths.wait_watch_log());
    rimz::harness::schedule::signal::run_watcher(&ctx.store, &ctx.workspace, name)
}
