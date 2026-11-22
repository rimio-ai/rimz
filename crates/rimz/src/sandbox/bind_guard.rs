//! Live repair of protected read-only binds that a host replacement detached from a running view.
//!
//! The guard thread counts against a scoped parked wrapper's memory budget, which only the live
//! `backend::tmux::agent_lifecycle::scoped_parked_wrapper_holds_host_budget_plus_bind_guard`
//! test measures. It self-skips where bubblewrap is unusable, CI included, so a change here runs
//! it locally under `cargo xtask test -P live`.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;

#[derive(Debug, thiserror::Error)]
pub enum BindGuardErr {
    #[error("scoped protected-file guard: {0}{hint}", hint = watch_limit(.0))]
    Init(#[source] io::Error),
    #[error(
        "scoped protected-file guard: watching {path}: {source}{hint}",
        hint = watch_limit(.source)
    )]
    Watch { path: PathBuf, source: io::Error },
    #[error("scoped protected-file guard: {0}{hint}", hint = watch_limit(.0))]
    Worker(#[source] io::Error),
}

fn watch_limit(error: &io::Error) -> &'static str {
    #[cfg(target_os = "linux")]
    {
        match error.raw_os_error() {
            Some(nix::libc::EMFILE) => "; raise fs.inotify.max_user_instances",
            Some(nix::libc::ENOSPC) => "; raise fs.inotify.max_user_watches",
            _ => "",
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = error;
        ""
    }
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
            #[cfg(any(test, feature = "testkit"))]
            if let Some(errno) = std::env::var("RIMZ_TEST_SCOPED_BIND_ARM_ERRNO")
                .ok()
                .and_then(|value| value.parse().ok())
            {
                return Err(BindGuardErr::Init(io::Error::from_raw_os_error(errno)));
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
