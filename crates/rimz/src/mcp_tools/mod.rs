//! The runner `rimz mcp serve` is handed: it carries out each tool call in
//! the server's process by calling the library function that owns the job,
//! the one the CLI command of the same job prints, and answers with the value
//! that function returns. Nothing here writes to stdout, which is the
//! protocol's channel, or ends the process, which serves every tool of the
//! agent. A process-wide memo outlives the call: a tool that writes what one
//! caches (the loop config's `LOOP_MEMO`, the machine config's `LOAD_MEMO`)
//! invalidates it, and a claim stamped with this process's pid reads as live
//! until the server exits. `tasks`, `join`, and `stop` act on what the agent
//! started, by an id `start` defines, and only on what `owned` traces back to
//! the caller.

mod control;
mod join;
mod layout;
mod lsp;
mod message;
mod notes;
mod notices;
mod owned;
mod room;
mod schedule;
mod start;
mod stop;
mod subagent;
mod tasks;
mod wait;
mod workflow;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::mcp::catalog::Call;
use crate::mcp::{Answer, Failure, Runner};
use crate::reload::RoomBuildFollower;

pub use start::{Start, TaskKind};

/// The tools of one server: its start directory, which a call's relative
/// `cwd` and a call without one resolve against, and the room its pane pin
/// names, both read once at its start, with the follower of that room's
/// build.
pub struct Tools {
    start_dir: PathBuf,
    pinned_room: Option<PathBuf>,
    follower: Option<Mutex<RoomBuildFollower>>,
}

impl Tools {
    /// The tools of a server whose start directory is `start_dir`, the
    /// launch directory `serve --launch-env` reads, or else the directory the
    /// process started in.
    pub fn new(start_dir: Option<PathBuf>) -> Arc<Self> {
        let pinned_room = crate::launcher::checkout::pinned_room_root();
        let follower = pinned_room.as_deref().map(|root| {
            Mutex::new(RoomBuildFollower::new(
                &crate::ids::WorkspaceId::from_project_root(root),
            ))
        });
        Arc::new(Self {
            start_dir: start_dir
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            pinned_room,
            follower,
        })
    }

    fn follower(&self) -> Option<MutexGuard<'_, RoomBuildFollower>> {
        let follower = self.follower.as_ref()?;
        Some(
            follower
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The directory a call runs in: its `cwd` against the start directory,
    /// or the start directory, as its physical path, the one a command started
    /// there reads from `getcwd`.
    fn dir(&self, cwd: Option<&Path>) -> Result<PathBuf, Failure> {
        let dir = cwd.map_or_else(|| self.start_dir.clone(), |cwd| self.start_dir.join(cwd));
        std::fs::canonicalize(&dir)
            .ok()
            .filter(|dir| dir.is_dir())
            .ok_or_else(|| Failure::new(format!("no directory {}", dir.display())))
    }
}

impl Runner for Tools {
    fn run(&self, call: Call, cancelled: &dyn Fn() -> bool) -> Answer {
        let answered = match call {
            Call::Lsp(args) => lsp::run(self, &args),
            Call::Wait(args) => wait::arm(self, &args),
            Call::Subagent(args) => subagent::launch(self, &args),
            Call::Agent(args) => layout::launch_agent(self, &args),
            Call::Team(args) => layout::launch_team(self, &args),
            Call::Workflow(args) => workflow::start(self, &args),
            Call::Schedule(args) => schedule::book(self, &args),
            Call::Message(args) => message::send(self, &args),
            Call::Tasks(args) => tasks::tasks(self, &args),
            Call::Join(args) => join::join(self, &args, cancelled),
            Call::Stop(args) => stop::stop(self, &args),
            Call::SelfStop(args) => control::stop(self, &args),
            Call::SelfCompact(args) => control::compact(self, &args),
            Call::SelfTune(args) => control::tune(self, &args),
            Call::SelfMolt(args) => control::molt(self, &args),
            Call::Notes(args) => notes::add(self, args),
            Call::Guide(_) => Err(Failure::new("the server answers guide itself")),
        };
        answered.unwrap_or_else(Answer::Failure)
    }

    fn poll_build(&self, now: Instant) -> Option<PathBuf> {
        self.follower()?.poll_if_due(now)
    }

    fn next_build_poll(&self) -> Option<Instant> {
        Some(self.follower()?.next_poll())
    }

    fn recheck_build(&self) -> Option<PathBuf> {
        self.follower()?.recheck()
    }
}

/// Replace this process with its own binary under the parent's variables the
/// server's environment lacks, so each call resolves the agent, home, and room
/// its parent names; a process that already holds every one of them goes on.
/// The parent's environment being unreadable, or the restart failing, leaves
/// the server on its own environment, with a warning.
pub fn restore_parent_env() {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let Some(parent) = crate::proc::environ(std::os::unix::process::parent_id()) else {
            tracing::warn!(
                "rimz mcp serve: the parent's environment is unreadable; calls run with the server's own"
            );
            return;
        };
        let lacking: Vec<(String, String)> = parent
            .into_iter()
            .filter(|(name, _)| std::env::var_os(name).is_none())
            .collect();
        if lacking.is_empty() {
            return;
        }
        let err = std::env::current_exe().map_or_else(
            |err| err,
            |exe| {
                std::process::Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .envs(lacking)
                    .exec()
            },
        );
        tracing::warn!(%err, "rimz mcp serve: restarting under the parent's environment; calls run with the server's own");
    }
}
