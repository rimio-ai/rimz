use super::*;

/// Spawn a hook-triggered `rimz` helper detached, with all stdio nulled (the
/// fresh-stdio invariant for hook helper children). The hook drops the child
/// into the shared reaper, so it returns before the helper runs and never adds
/// latency to the agent's turn. Best-effort: a spawn failure is logged and
/// ignored; durable queue work remains pending for a later transition.
pub(super) fn spawn_refresh_detached(spawn: &rimz::agents::RefreshSpawn) {
    let exe = rimz::proc::rimz_exe();
    let mut cmd = Command::new(exe);
    cmd.args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = rimz::child_process::spawn_detached_reaped(&mut cmd, "adapter-refresh") {
        warn!(error = %err, "lifecycle: failed to spawn the adapter refresh helper");
    }
}
