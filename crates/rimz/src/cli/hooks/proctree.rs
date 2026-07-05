use super::*;

#[cfg(target_os = "linux")]
pub(super) fn walk_to_agent_ancestor(source: &str) -> Option<u32> {
    // Cap the walk so a pathologically deep tree (or a /proc parse glitch)
    // cannot loop the hook helper. 32 levels is far beyond any real agent
    // launch chain.
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..32 {
        if pid <= 1 {
            return None;
        }
        let (name, ppid) = read_proc_status(pid)?;
        if matches_agent_kind(&name, source) {
            return Some(pid);
        }
        pid = ppid;
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub(super) fn walk_to_agent_ancestor(_source: &str) -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn read_proc_status(pid: u32) -> Option<(String, u32)> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut name = None;
    let mut ppid = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("Name:") {
            name = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("PPid:") {
            ppid = rest.trim().parse::<u32>().ok();
        }
        if name.is_some() && ppid.is_some() {
            break;
        }
    }
    Some((name?, ppid?))
}

/// Whether the kernel-reported `comm` is one of the agent's declared process
/// names — its own binary plus any launcher (Codex ships as a JS bundle, so
/// its `comm` is `node`) or a target-triple release-binary name. The set lives
/// on the descriptor; an unregistered kind falls back to the exact-name match.
pub(super) fn matches_agent_kind(comm: &str, source: &str) -> bool {
    match rimz::agents::descriptor_by_kind(source) {
        Some(descriptor) => descriptor.runs_as(comm),
        None => comm == source,
    }
}

/// Verified pin roots from live processes of this agent kind at the hook's
/// cwd — the recovery source for a daemon-routed hook ([`run_feed`]). The
/// agent spawned its hook with the session cwd, so the in-pane agent process
/// sharing that cwd carries the pane's pin. Every declared process name is a
/// candidate — launchers included (`node` for Codex's JS bundle), so an
/// unrelated same-name process at the same cwd can enter the scan; the
/// per-candidate [`workspace::verify_pin`] and the resolver's all-agree rule
/// keep a stray candidate degrading to the static ladder rather than
/// misrouting. Predicates run cheap-first (`comm`, then the `cwd` readlink,
/// then the two `environ` reads) so only this agent's processes pay the
/// `/proc` reads; non-Linux yields nothing and the resolver falls back to
/// the static ladder.
pub(super) fn sibling_agent_pins(source: &str, cwd: &Path) -> Vec<PathBuf> {
    rimz::proc::list_processes()
        .into_iter()
        .filter(|info| {
            rimz::proc::comm(info.pid).is_some_and(|comm| matches_agent_kind(&comm, source))
        })
        .filter(|info| rimz::proc::cwd(info.pid).is_some_and(|proc_cwd| proc_cwd == cwd))
        .filter_map(|info| {
            let id = rimz::proc::env_var(info.pid, workspace::ENV_WORKSPACE_ID)?;
            let root = rimz::proc::env_var(info.pid, workspace::ENV_PROJECT_ROOT)?;
            workspace::verify_pin(&id, Path::new(&root))
        })
        .collect()
}
