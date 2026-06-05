//! Reload (`rimz reload` or the `r` keypress): resolve the on-disk renderer
//! binary, compare it to the running image, and re-exec in place only when the
//! build actually changed. Also resolves the `rimz` binary the detach helper
//! drives, healing a deleted dev-worktree path via `PATH`.

use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::SidebarAppErr;

/// Replace this process with a fresh invocation of `exe` and our own argv.
/// After `rimz reload`, the renderer's binary on disk has been updated in
/// place; re-execing the resolved path loads the new code without touching the
/// pane or session. Only returns on failure — success replaces the image.
pub(super) fn reexec_self(exe: &Path) -> SidebarAppErr {
    use std::os::unix::process::CommandExt;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = Command::new(exe).args(&args).exec();
    SidebarAppErr::CommandIo {
        program: exe.display().to_string(),
        source,
    }
}

/// What a reload (`rimz reload` or the `r` keypress) does this tick: re-exec
/// onto a changed on-disk binary, skip the re-exec when it is byte-identical to
/// the running image, or keep the current build when nothing is on disk to load.
pub(super) enum ReloadAction {
    /// The on-disk binary differs from the running image — load it in place.
    Reexec(PathBuf),
    /// The on-disk binary is byte-identical to the running image — skip the
    /// re-exec churn and refetch in place instead.
    AlreadyCurrent,
    /// No binary resolves on disk (a partial or in-flight install) — keep
    /// serving the current build and refetch.
    Missing,
}

/// Decide the reload action from the resolved target and whether it matches the
/// running image. Pure, so the branching is unit-tested directly. An unknown
/// match (the running image's bytes were unreadable) re-execs, preserving the
/// always-load-the-on-disk-build behavior.
fn decide_reload(target: Option<PathBuf>, running_matches: Option<bool>) -> ReloadAction {
    match (target, running_matches) {
        (None, _) => ReloadAction::Missing,
        (Some(_), Some(true)) => ReloadAction::AlreadyCurrent,
        (Some(target), Some(false) | None) => ReloadAction::Reexec(target),
    }
}

/// Resolve this reload into an action: find the binary to load, then compare it
/// to the running image so an unchanged build skips the re-exec.
pub(super) fn reload_action() -> ReloadAction {
    let target = reexec_target();
    let running_matches = target.as_deref().and_then(running_image_matches);
    decide_reload(target, running_matches)
}

/// Resolve the on-disk binary to re-exec for a reload, or `None` when none can
/// be found — in which case the caller keeps serving the current build instead
/// of vanishing.
fn reexec_target() -> Option<PathBuf> {
    resolve_reexec_target(std::env::current_exe().ok()?)
}

/// Pick the live binary behind a `current_exe()` reading.
///
/// A fresh `cargo install` replaces our binary via atomic rename, which unlinks
/// the inode the running process still holds. The kernel then annotates
/// `/proc/self/exe` (what `current_exe()` reads) with a trailing " (deleted)",
/// so the raw path no longer resolves on disk. The replacement now lives at the
/// un-annotated path — exactly the build `rimz reload` means to pick up — so we
/// strip that marker and prefer whichever path is a real file. `None` (neither
/// path exists, e.g. a partial install) tells the caller to keep the old build.
fn resolve_reexec_target(exe: PathBuf) -> Option<PathBuf> {
    if exe.is_file() {
        return Some(exe);
    }
    strip_deleted_suffix(&exe).filter(|path| path.is_file())
}

/// Resolve the `rimz` binary the detach helper drives (`rimz pane detach`).
///
/// `cached` is the path captured at launch — the sibling `rimz` beside this
/// renderer, or `RIMZ_BIN`. A long-lived sidebar can outlive it: removing the
/// dev worktree it was built in deletes that binary out from under the still
/// running renderer, and the helper would then fail with ENOENT (a reload
/// cannot rescue it either, since the renderer binary in that worktree is gone
/// too). Keep the cached path while it is a real file; once it vanishes, fall
/// back to the installed `rimz` on `PATH` so the helper keeps working.
pub(super) fn resolve_rimz_bin(cached: &Path) -> PathBuf {
    if cached.is_file() {
        return cached.to_path_buf();
    }
    // A bare name; `Command::new` resolves it against `PATH`.
    PathBuf::from(format!("rimz{}", std::env::consts::EXE_SUFFIX))
}

/// Strip the kernel's " (deleted)" annotation from a `/proc/self/exe` path.
/// `None` when the path carries no such suffix.
fn strip_deleted_suffix(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    const DELETED_SUFFIX: &[u8] = b" (deleted)";
    let stripped = path.as_os_str().as_bytes().strip_suffix(DELETED_SUFFIX)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(stripped)))
}

/// Whether the binary at `target` is byte-identical to the image this process
/// is currently running. `None` when the running image's bytes can't be read —
/// no `/proc/self/exe` (non-Linux) or an IO race — in which case the caller
/// re-execs unconditionally, preserving the always-load-the-on-disk-build
/// behavior.
fn running_image_matches(target: &Path) -> Option<bool> {
    let running = running_image_path()?;
    same_file_contents(&running, target).ok()
}

/// Path that reads back the bytes of the image this process is executing. Linux
/// exposes it as `/proc/self/exe`, which resolves to the running inode even
/// after an atomic-rename install has unlinked it from its original path — so a
/// post-install renderer can still read the build it is running.
#[cfg(target_os = "linux")]
fn running_image_path() -> Option<PathBuf> {
    Some(PathBuf::from("/proc/self/exe"))
}

/// No `/proc` to read the running image from, so reload always re-execs.
#[cfg(not(target_os = "linux"))]
fn running_image_path() -> Option<PathBuf> {
    None
}

/// Whether two files hold byte-identical content. A size mismatch is an
/// immediate `false`; otherwise both streams are read in lockstep chunks and
/// the compare early-exits on the first difference, so no whole binary is ever
/// buffered.
fn same_file_contents(a: &Path, b: &Path) -> io::Result<bool> {
    if std::fs::metadata(a)?.len() != std::fs::metadata(b)?.len() {
        return Ok(false);
    }
    let mut reader_a = io::BufReader::new(std::fs::File::open(a)?);
    let mut reader_b = io::BufReader::new(std::fs::File::open(b)?);
    let mut buf_a = [0u8; 8192];
    let mut buf_b = [0u8; 8192];
    loop {
        let read_a = fill(&mut reader_a, &mut buf_a)?;
        let read_b = fill(&mut reader_b, &mut buf_b)?;
        if read_a != read_b {
            // Equal lengths were confirmed above; a differing fill here means a
            // concurrent truncate — treat as not-identical.
            return Ok(false);
        }
        if read_a == 0 {
            return Ok(true);
        }
        if buf_a[..read_a] != buf_b[..read_b] {
            return Ok(false);
        }
    }
}

/// Read up to `buf.len()` bytes, looping past short reads and `Interrupted`
/// until the buffer is full or EOF. Returns how many bytes were read.
fn fill(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests;
