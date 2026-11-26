//! The file a result-file subagent writes its result to (`RunRecord::result_file`): the bounded
//! read a terminal write takes into `last_message`, its removal, the path as the subagent sees
//! it, and the one reminder a turn end with no result gets.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::Store;
use crate::disk::paths::StatePaths;
use crate::ids::RunId;
use crate::store::message::{DeliveryGate, MessageRecord, MessageSender};
use crate::store::run::RunRecord;

/// The most of a result file a terminal write takes.
const CAP: usize = 1024 * 1024;
const CUT_LINE: &str = "\n\n[result cut at 1 MiB]";

/// The result at `path`: a regular file's text as lossy UTF-8, cut to at most `CAP` bytes of
/// text with `CUT_LINE` after it, and none when that text is blank. The subagent controls the
/// path and the terminal write reads it under the workspace lock, so the open follows no symlink
/// and never blocks on a FIFO, and "regular" is asked of the open handle. A failure warns and
/// reads as no result.
pub(super) fn read(path: &Path) -> Option<String> {
    let read = || -> std::io::Result<Option<Vec<u8>>> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        file.take(CAP as u64 + 1).read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    };
    let mut bytes = match read() {
        Ok(bytes) => bytes?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read a subagent's result file");
            return None;
        }
    };
    let read_past_cap = bytes.len() > CAP;
    if read_past_cap {
        bytes.truncate(CAP);
        // A character the cap split is dropped whole rather than read as a replacement.
        if let Err(error) = std::str::from_utf8(&bytes)
            && error.error_len().is_none()
        {
            bytes.truncate(error.valid_up_to());
        }
    }
    // An invalid byte decodes to a three-byte replacement, so the cap applies again to the text.
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let cut = read_past_cap || text.len() > CAP;
    let mut end = text.len().min(CAP);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    if text.trim().is_empty() {
        return None;
    }
    if cut {
        text.push_str(CUT_LINE);
    }
    Some(text)
}

/// Whether run `run_id` has a result file holding a result now, read with no lock held. A run
/// without one, or whose record cannot be read, holds none.
pub(super) fn written(paths: &StatePaths, run_id: &RunId) -> bool {
    super::load(paths, run_id)
        .ok()
        .and_then(|record| record.result_file)
        .is_some_and(|path| read(&path).is_some())
}

/// `record`'s result file as its subagent sees it, through the temp view of the subagent's row:
/// `/tmp/<name>.output` in a scoped view, the host path otherwise or when no row is found. Reads
/// the room's snapshot and machine config, so call it with no workspace lock held.
pub fn seen(store: &Store, record: &RunRecord) -> Option<PathBuf> {
    let host = record.result_file.as_deref()?;
    let snapshot = store
        .snapshot_cached()
        .inspect_err(|error| {
            tracing::debug!(run_id = %record.run_id, %error, "result file view: no snapshot");
        })
        .ok();
    let row = snapshot
        .as_ref()
        .zip(record.agent_id.as_ref())
        .and_then(|(snapshot, id)| {
            crate::store::snapshot::find_agent(&snapshot.agents, record.kind.as_str(), id)
        });
    let (Some(snapshot), Some(row)) = (&snapshot, row) else {
        return Some(host.to_path_buf());
    };
    let isolation = row.runs_in(
        crate::config::MachineConfig::load_lenient()
            .agents
            .isolation,
    );
    let owner = crate::harness::launch_plan::agent_temp_owner(row, snapshot.agents.iter());
    Some(crate::sandbox::TmpView::current(isolation, owner, store.paths()).agent_path(host))
}

/// Queue the reminder notice of a run whose open answer waits on it
/// (`RunRecord::pending_result_reminder`), once by its id: a System notice into the subagent's
/// pane, gated on its rest and linked to the run, so one spent or replaced before delivery is
/// withdrawn (`message::premise`). Anything else queues nothing. Returns whether the notice is
/// queued, by this call or an earlier one.
pub fn queue_reminder(
    store: &Store,
    session_name: &str,
    run_id: &RunId,
) -> crate::store::Result<bool> {
    let Ok(record) = super::load(store.paths(), run_id) else {
        return Ok(false);
    };
    let (Some(message_id), Some(agent_id), Some(path)) = (
        record.pending_result_reminder(),
        record.agent_id.as_ref(),
        seen(store, &record),
    ) else {
        return Ok(false);
    };
    let snapshot = store.snapshot_cached()?;
    let Some(row) =
        crate::store::snapshot::find_agent(&snapshot.agents, record.kind.as_str(), agent_id)
    else {
        tracing::warn!(%run_id, "result reminder: the subagent has no row to queue it for");
        return Ok(false);
    };
    let mut notice = MessageRecord::new(
        record.workspace_id.clone(),
        row,
        reminder_text(&path),
        DeliveryGate::Done,
    )
    .with_channel(row.lane_name())
    .with_sender(MessageSender::System)
    .with_about_run(Some(run_id.clone()));
    notice.message_id = message_id.clone();
    store.queue_message_once(&notice, session_name)?;
    Ok(true)
}

/// Queue run `run_id`'s reminder notice again, under the room's session name, for a check that
/// found it missing: whether the park should keep waiting on it. A failure holds the park, as an
/// unreadable queue does; a subagent with no row to remind lets it settle.
pub(super) fn requeue_reminder(store: &Store, run_id: &RunId) -> bool {
    let queued = crate::workspace::record::read(&store.paths().workspace_record)
        .map_err(|error| error.to_string())
        .and_then(|room| {
            queue_reminder(store, &room.session_name, run_id).map_err(|error| error.to_string())
        });
    queued.unwrap_or_else(|error| {
        tracing::warn!(%run_id, %error, "result reminder: could not queue the missing notice");
        true
    })
}

fn reminder_text(path: &Path) -> String {
    format!(
        "Your launcher has no result for this task yet: `{}` has not been written since the task \
         arrived. It reads only that file. Write your whole result there now, then end your turn. \
         If the task is not finished, write what you have and what is left.",
        path.display()
    )
}

/// Remove the result file at `path`; a failure warns.
pub(crate) fn remove(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not remove a subagent's result file");
        }
    }
}
