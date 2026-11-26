//! The file a result-file subagent writes its result to (`RunRecord::result_file`): the bounded
//! read a terminal write takes into `last_message`, and its removal.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

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
