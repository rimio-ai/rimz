//! Live repair of protected read-only binds that a host replacement detached from a running view.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;

#[derive(Debug, thiserror::Error)]
pub enum BindGuardErr {
    #[error("scoped protected-file guard: {0}")]
    Init(#[source] io::Error),
    #[error("scoped protected-file guard: watching {path}: {source}")]
    Watch { path: PathBuf, source: io::Error },
    #[error("scoped protected-file guard: {0}")]
    Worker(#[source] io::Error),
}

#[derive(Debug)]
pub struct BindRestore {
    pub paths: Vec<PathBuf>,
    pub result: Result<(), String>,
}

pub struct BindGuard {
    #[cfg(target_os = "linux")]
    inner: linux::Guard,
}

impl BindGuard {
    pub fn arm(
        entries: &[PathBuf],
        helper: &Path,
        sink: impl Fn(BindRestore) + Send + 'static,
    ) -> Result<Option<Self>, BindGuardErr> {
        #[cfg(target_os = "linux")]
        {
            if entries.is_empty() {
                return Ok(None);
            }
            Ok(Some(Self {
                inner: linux::Guard::arm(entries, helper, sink)?,
            }))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (entries, helper, sink);
            Ok(None)
        }
    }

    pub fn point(&self, pid: u32) {
        #[cfg(target_os = "linux")]
        self.inner.point(pid);
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
    }

    pub fn prepare_handoff(&mut self) -> Result<i32, BindGuardErr> {
        #[cfg(target_os = "linux")]
        {
            self.inner.prepare_handoff()
        }
        #[cfg(not(target_os = "linux"))]
        Err(BindGuardErr::Worker(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }

    pub fn handover(
        inherited: Option<i32>,
        entries: &[PathBuf],
        helper: &Path,
        sink: impl Fn(BindRestore) + Send + 'static,
    ) -> Result<Option<Self>, BindGuardErr> {
        #[cfg(target_os = "linux")]
        {
            let Some(fd) = inherited else {
                return Ok(None);
            };
            Ok(Some(Self {
                inner: linux::Guard::handover(fd, entries, helper, sink)?,
            }))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (inherited, entries, helper, sink);
            Ok(None)
        }
    }
}

#[cfg(target_os = "linux")]
pub fn run_bind_helper() -> Option<std::process::ExitCode> {
    linux::run_bind_helper()
}
