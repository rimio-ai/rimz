//! The launch child of a workflow, agent or team fire: `rimz` run with the task's stored launch
//! argv in the fire's checkout. Its environment carries the fire's stamps with the room's
//! channel and any agent identity dropped, its stdin is null, and its output goes to a log file,
//! never a pipe, since a `-p` child outlives the fire.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use super::TaskLaunch;
use crate::agents::LaunchedBy;
use crate::harness::loop_task::{
    LOOP_BOOKED_BY_ENV, LOOP_FIRE_ENV, LOOP_TASK_ENV, LOOP_TRIGGER_ENV,
};
use crate::ids::{FireId, RunId, WorkflowRunId};

const POLL: Duration = Duration::from_millis(100);
/// How long a launch log is kept after its fire.
const LOG_KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The most of a log's end a refused launch's history row carries.
const TAIL_BYTES: u64 = 2048;
/// How far into a launch log the run id a `workflow run` child prints first is looked for,
/// past any warnings its stderr wrote ahead of it.
const LINES_TO_RUN_ID: usize = 64;

/// What one fire's launch child runs, and under which stamps.
pub(super) struct Request<'a> {
    pub(super) launch: &'a TaskLaunch,
    pub(super) fire: &'a FireId,
    /// The task's name at the fire.
    pub(super) task: &'a str,
    pub(super) checkout: &'a Path,
    /// What fired it, as loop words it for a launch prompt; empty for none.
    pub(super) trigger: &'a str,
    pub(super) booked_by: &'a [LaunchedBy],
    /// A workflow fire's start, handed over as `--loop-fire-json`.
    pub(super) start: Option<&'a crate::harness::workflow::TaskStart>,
}

/// A spawned launch child the fire waits on.
pub(super) struct Spawned {
    child: Child,
    log: PathBuf,
}

/// How the fire's wait on its launch child ended.
pub(super) enum Ended {
    /// The child exited with `code` (none when a signal ended it), leaving `tail` in its log
    /// and, for a `workflow run` child, the id it printed: the run it published, or on exit 122
    /// the live run that refused it.
    Exited {
        code: Option<i32>,
        tail: String,
        run: Option<WorkflowRunId>,
    },
    /// An agent fire's supervised run carrying the fire is published; the child runs it on.
    Published(RunId),
}

/// Spawn the launch child in its own process group, so a terminal's interrupt to a `loop fire`
/// by hand does not reach it.
pub(super) fn spawn(request: &Request<'_>) -> Result<Spawned> {
    let dir = crate::disk::paths::loop_launch_logs_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    prune_logs(&dir);
    let log = dir.join(format!("{}.log", request.fire));
    let out = File::create(&log).with_context(|| format!("creating {}", log.display()))?;
    let err = out.try_clone()?;
    let mut command = Command::new(crate::proc::rimz_exe());
    command
        .args(request.launch.argv())
        .current_dir(request.checkout)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    if let Some(start) = request.start {
        command
            .arg("--loop-fire-json")
            .arg(serde_json::to_string(start)?);
    }
    crate::harness::check_run::drop_room_identity(&mut command);
    crate::child_process::restore_user_temp_env(&mut command);
    command
        .env(LOOP_FIRE_ENV, request.fire.as_str())
        .env(LOOP_TASK_ENV, request.task)
        .env(crate::workspace::ENV_WORKTREE_PATH, request.checkout);
    match request.trigger {
        "" => command.env_remove(LOOP_TRIGGER_ENV),
        trigger => command.env(LOOP_TRIGGER_ENV, trigger),
    };
    match serde_json::to_string(request.booked_by) {
        Ok(chain) if !request.booked_by.is_empty() => command.env(LOOP_BOOKED_BY_ENV, chain),
        _ => command.env_remove(LOOP_BOOKED_BY_ENV),
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .with_context(|| format!("spawning `rimz {}`", request.launch.argv().join(" ")))?;
    Ok(Spawned { child, log })
}

impl Spawned {
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for the child to exit, or, for an agent fire, until `published` finds the
    /// supervised run carrying the fire.
    pub(super) fn wait(mut self, published: impl Fn() -> Option<RunId>) -> Result<Ended> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Ended::Exited {
                    code: status.code(),
                    tail: log_tail(&self.log),
                    run: printed_run(&self.log),
                });
            }
            if let Some(run) = published() {
                return Ok(Ended::Published(run));
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The end of a launch log, as one trimmed text.
fn log_tail(path: &Path) -> String {
    let read = || -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    };
    read().unwrap_or_default()
}

/// The run id a `workflow run` child printed: the first log line that reads as one.
fn printed_run(path: &Path) -> Option<WorkflowRunId> {
    BufReader::new(File::open(path).ok()?)
        .lines()
        .take(LINES_TO_RUN_ID)
        .map_while(std::result::Result::ok)
        .find_map(|line| WorkflowRunId::parse(line.trim()).ok())
}

/// Remove launch logs older than [`LOG_KEEP`]; a log that cannot be read or removed stays.
fn prune_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > LOG_KEEP);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
