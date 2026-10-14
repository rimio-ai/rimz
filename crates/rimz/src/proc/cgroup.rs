//! Cgroup-v2 agent scopes. Scope paths carry the injected cgroup root.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CgroupErr {
    #[error("cgroup hibernation requires Linux")]
    Unsupported,
    #[error("invalid agent scope: {0}")]
    InvalidScope(PathBuf),
    #[error("cgroup did not freeze within two seconds")]
    FreezeTimeout,
    #[error(transparent)]
    Io(#[from] io::Error),
}

type Result<T> = std::result::Result<T, CgroupErr>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopePath(PathBuf);

impl ScopePath {
    /// Resolve this user's agent scope from cgroup membership under `cgroup_root`.
    pub fn from_cgroup(cgroup_root: &Path, membership: &str) -> Option<Self> {
        scope_in(
            cgroup_root,
            membership,
            nix::unistd::Uid::current().as_raw(),
        )
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimReport {
    pub before: u64,
    pub after: u64,
}

const ROOT: &str = "/sys/fs/cgroup";

fn user_app_slice_in(root: &Path, uid: u32) -> PathBuf {
    root.join(format!(
        "user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ))
}

pub(crate) fn thaw_workspace(workspace: &crate::ids::WorkspaceId) -> Result<()> {
    thaw_workspace_in(Path::new(ROOT), workspace)
}

pub(crate) fn thaw_workspace_in(root: &Path, workspace: &crate::ids::WorkspaceId) -> Result<()> {
    for scope in workspace_scopes_in(root, workspace)? {
        match thaw(&scope) {
            Err(CgroupErr::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            result => result?,
        }
    }
    Ok(())
}

pub(crate) fn workspace_scopes_in(
    root: &Path,
    workspace: &crate::ids::WorkspaceId,
) -> Result<Vec<ScopePath>> {
    if !cfg!(target_os = "linux") {
        return Ok(Vec::new());
    }
    let uid = nix::unistd::Uid::current().as_raw();
    let slice = user_app_slice_in(root, uid);
    let entries = match std::fs::read_dir(slice) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let prefix = scope_prefix(workspace);
    let mut scopes = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_dir()
            && name
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".scope"))
        {
            scopes.push(ScopePath(entry.path()));
        }
    }
    Ok(scopes)
}

/// Prefix shared by launch and workspace-scoped reconciliation.
pub fn scope_prefix(workspace: &crate::ids::WorkspaceId) -> String {
    let id: String = workspace
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("rimz-agent-{id}-")
}

fn app_slice_in(root: &Path, text: &str, uid: u32) -> Option<PathBuf> {
    let group = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    let relative = Path::new(group.strip_prefix('/')?);
    if relative
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let service = format!("user.slice/user-{uid}.slice/user@{uid}.service");
    relative.strip_prefix(&service).ok()?;
    Some(root.join(service).join("app.slice"))
}

fn scope_in(root: &Path, text: &str, uid: u32) -> Option<ScopePath> {
    let slice = app_slice_in(root, text, uid)?;
    let group = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    let path = root.join(group.strip_prefix('/')?);
    let name = path.file_name()?.to_str()?;
    (path.parent() == Some(slice.as_path())
        && name.starts_with("rimz-agent-")
        && name.ends_with(".scope"))
    .then_some(ScopePath(path))
}

pub fn own_scope() -> Option<ScopePath> {
    scope_of(std::process::id())
}

pub fn scope_of(pid: u32) -> Option<ScopePath> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    ScopePath::from_cgroup(Path::new(ROOT), &text)
}

pub fn user_app_slice() -> Result<PathBuf> {
    supported()?;
    Ok(user_app_slice_in(
        Path::new(ROOT),
        nix::unistd::Uid::current().as_raw(),
    ))
}

fn supported() -> Result<()> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(CgroupErr::Unsupported)
    }
}

pub fn freeze(scope: &ScopePath) -> Result<()> {
    freeze_with(scope, Duration::from_secs(2))
}
fn freeze_with(scope: &ScopePath, timeout: Duration) -> Result<()> {
    supported()?;
    std::fs::write(scope.0.join("cgroup.freeze"), "1")?;
    let deadline = std::time::Instant::now() + timeout;
    let result = loop {
        match std::fs::read_to_string(scope.0.join("cgroup.events")) {
            Ok(events) if events.lines().any(|line| line == "frozen 1") => break Ok(()),
            Err(error) => break Err(error.into()),
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            break Err(CgroupErr::FreezeTimeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if result.is_err() {
        thaw(scope)?;
    }
    result
}

pub fn thaw(scope: &ScopePath) -> Result<()> {
    supported()?;
    Ok(std::fs::write(scope.0.join("cgroup.freeze"), "0")?)
}

pub fn reclaim(scope: &ScopePath, bytes: u64) -> Result<ReclaimReport> {
    supported()?;
    let current = || -> io::Result<u64> {
        std::fs::read_to_string(scope.0.join("memory.current"))?
            .trim()
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    };
    let before = current()?;
    if bytes > 0 {
        reclaim_with(bytes, |value| {
            std::fs::write(scope.0.join("memory.reclaim"), value)
        })?;
    }
    Ok(ReclaimReport {
        before,
        after: current()?,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct MemoryStat {
    pub anon: u64,
    pub file: u64,
    pub workingset_refault_anon: u64,
    pub workingset_refault_file: u64,
}

pub fn memory_stat(scope: &ScopePath) -> Result<MemoryStat> {
    supported()?;
    let text = std::fs::read_to_string(scope.0.join("memory.stat"))?;
    let read = |key: &str| -> Result<u64> {
        text.lines()
            .find_map(|line| {
                let (name, value) = line.split_once(' ')?;
                (name == key).then_some(value)
            })
            .and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("missing or invalid {key} in memory.stat"),
                )
                .into()
            })
    };
    Ok(MemoryStat {
        anon: read("anon")?,
        file: read("file")?,
        workingset_refault_anon: read("workingset_refault_anon")?,
        workingset_refault_file: read("workingset_refault_file")?,
    })
}

pub fn procs(scope: &ScopePath) -> Vec<u32> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    std::fs::read_to_string(scope.0.join("cgroup.procs"))
        .ok()
        .and_then(|text| {
            text.lines()
                .map(str::parse)
                .collect::<std::result::Result<Vec<u32>, _>>()
                .ok()
        })
        .unwrap_or_default()
}

fn reclaim_with(bytes: u64, mut write: impl FnMut(&str) -> io::Result<()>) -> io::Result<()> {
    let result = match write(&format!("{bytes} swappiness=200")) {
        Err(error) if error.raw_os_error() == Some(nix::libc::EINVAL) => write(&bytes.to_string()),
        result => result,
    };
    match result {
        Err(error) if error.raw_os_error() == Some(nix::libc::EAGAIN) => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests;
