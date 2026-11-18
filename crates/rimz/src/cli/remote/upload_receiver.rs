//! `rimz remote upload`: the host side of a remote paste. The laptop's upload
//! client runs it over the control socket, sends one header line and the
//! files' bytes, and reads back the paths the files landed at.
//!
//! Each transfer is its own temp unit, `tmp/_upload-<id>/`, which GC keeps for
//! the temp grace after its last write. The receiver alone writes the room's
//! upload lane, the sidebar's progress row, and removes it when it ends.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use rimz::remote::upload::{UploadHeader, UploadLane, UploadReply};

use super::link_stats::{RoomArgs, room_store};

/// A transfer that sends nothing for this long is cut short.
const IDLE_LIMIT: Duration = Duration::from_secs(30);
/// The progress row moves at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const SCHEMA_MISMATCH_EXIT: i32 = 2;

pub(super) fn run(args: RoomArgs) -> Result<()> {
    let (store, _) = room_store(args)?;
    let runtime = store.runtime_paths();
    runtime
        .ensure_dirs()
        .context("preparing runtime directories for the upload lane")?;
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut line = String::new();
    input
        .read_line(&mut line)
        .context("reading upload header")?;
    let header: UploadHeader = serde_json::from_str(&line).context("parsing upload header")?;
    if !header.version_ok() {
        let _ = writeln!(
            std::io::stderr().lock(),
            "unsupported upload schema `{}`",
            header.v
        );
        std::process::exit(SCHEMA_MISMATCH_EXIT);
    }
    let lane = Lane {
        runtime,
        path: rimz::remote::upload::lane_path(runtime),
        id: transfer_id(),
        total: header.total(),
    };
    if header.total() > header.limit {
        let reason = format!(
            "{} is over the {} upload limit (paste_max)",
            rimz::theme::fmt::fmt_bytes(header.total()),
            rimz::theme::fmt::fmt_bytes(header.limit)
        );
        return lane.fail(reason);
    }
    let unit = match store.paths().ensure_upload_unit(&lane.id) {
        Ok(unit) => unit,
        Err(err) => return lane.fail(format!("the room's temp folder is unusable: {err}")),
    };
    lane.publish(0);
    reply(&UploadReply::ready())?;
    let names = file_names(&header);
    let received = receive(&mut input, &header, &unit, &names, &lane);
    let paths = match received {
        Ok(paths) => paths,
        Err(Receive::Cut(err)) => {
            let _ = std::fs::remove_dir_all(&unit);
            lane.remove_if_owned();
            return Err(err);
        }
        Err(Receive::Failed(reason)) => {
            let _ = std::fs::remove_dir_all(&unit);
            return lane.fail(reason);
        }
    };
    if let Some(missing) = paths.iter().find(|path| !path.is_file()) {
        let _ = std::fs::remove_dir_all(&unit);
        return lane.fail(format!("{} vanished before the paste", missing.display()));
    }
    lane.remove_if_owned();
    reply(&UploadReply::done(
        paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    ))
}

enum Receive {
    /// The laptop stopped sending: a cancel, a dropped link, or a local error.
    Cut(anyhow::Error),
    /// The host could not keep the files.
    Failed(String),
}

fn receive(
    input: &mut BufReader<std::io::StdinLock<'_>>,
    header: &UploadHeader,
    unit: &Path,
    names: &[String],
    lane: &Lane<'_>,
) -> std::result::Result<Vec<PathBuf>, Receive> {
    let mut received = 0_u64;
    let mut published = Instant::now();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut paths = Vec::new();
    for (file, name) in header.files.iter().zip(names) {
        let path = unit.join(name);
        let mut out = std::fs::File::create(&path)
            .map_err(|err| Receive::Failed(format!("could not write {name} on the host: {err}")))?;
        let mut left = file.size;
        while left > 0 {
            let want = buffer
                .len()
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            let read = read_within(input, &mut buffer[..want]).map_err(Receive::Cut)?;
            out.write_all(&buffer[..read]).map_err(|err| {
                Receive::Failed(format!("could not write {name} on the host: {err}"))
            })?;
            left -= read as u64;
            received += read as u64;
            if received < lane.total && published.elapsed() >= PROGRESS_EVERY {
                lane.publish(received);
                published = Instant::now();
            }
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Reads at least one byte, or fails when the laptop closed the stream or
/// sent nothing for [`IDLE_LIMIT`].
fn read_within(input: &mut BufReader<std::io::StdinLock<'_>>, buffer: &mut [u8]) -> Result<usize> {
    if input.buffer().is_empty() {
        let stdin = std::io::stdin();
        let mut fds = [nix::poll::PollFd::new(
            stdin.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        let timeout =
            nix::poll::PollTimeout::try_from(IDLE_LIMIT).unwrap_or(nix::poll::PollTimeout::MAX);
        if nix::poll::poll(&mut fds, timeout).context("waiting for upload bytes")? == 0 {
            bail!("upload stalled: no bytes for {}s", IDLE_LIMIT.as_secs());
        }
    }
    match input.read(buffer).context("reading upload bytes")? {
        0 => bail!("upload ended before its last byte"),
        read => Ok(read),
    }
}

/// The upload lane of one transfer.
struct Lane<'a> {
    runtime: &'a rimz::RuntimePaths,
    path: PathBuf,
    id: String,
    total: u64,
}

impl Lane<'_> {
    fn publish(&self, received: u64) {
        let record = UploadLane::running(
            self.id.clone(),
            received,
            self.total,
            rimz::utils::time::unix_now_ms(),
        );
        self.write(&record);
    }

    /// Replies the failure, holds its row for the sidebar, then removes it.
    fn fail(&self, reason: String) -> Result<()> {
        let record = UploadLane::failed(
            self.id.clone(),
            self.total,
            reason.clone(),
            rimz::utils::time::unix_now_ms(),
        );
        self.write(&record);
        reply(&UploadReply::failed(reason))?;
        std::thread::sleep(rimz::remote::upload::UPLOAD_FAILED_HOLD);
        self.remove_if_owned();
        Ok(())
    }

    fn write(&self, record: &UploadLane) {
        if let Err(err) = rimz::disk::atomic::write_temp_then_rename_cache(&self.path, record) {
            tracing::debug!(path = %self.path.display(), error = %err, "upload lane write failed");
            return;
        }
        let _ = rimz::wakeup::wake_store_delta(self.runtime, None, None);
    }

    fn remove_if_owned(&self) {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return;
        };
        let owned =
            serde_json::from_slice::<UploadLane>(&bytes).is_ok_and(|lane| lane.id == self.id);
        if owned && std::fs::remove_file(&self.path).is_ok() {
            let _ = rimz::wakeup::wake_store_delta(self.runtime, None, None);
        }
    }
}

fn reply(line: &UploadReply) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, line).context("writing upload reply")?;
    writeln!(stdout).context("writing upload reply")?;
    stdout.flush().context("flushing upload reply")
}

/// Unique per transfer on this host and safe in a path.
fn transfer_id() -> String {
    format!(
        "{:x}-{}",
        rimz::utils::time::unix_now_ms(),
        std::process::id()
    )
}

/// Each file's base name as it lands: control characters and `/` replaced,
/// never empty, `.` or `..`, and a repeated name numbered before its
/// extension.
fn file_names(header: &UploadHeader) -> Vec<String> {
    let mut taken = BTreeSet::new();
    header
        .files
        .iter()
        .map(|file| {
            let base = file.name.rsplit('/').next().unwrap_or_default();
            let clean: String = base
                .chars()
                .map(|ch| if ch.is_control() { '_' } else { ch })
                .collect();
            let clean = match clean.as_str() {
                "" | "." | ".." => "file".to_owned(),
                _ => clean,
            };
            let (stem, extension) = match clean.rfind('.') {
                Some(dot) if dot > 0 => clean.split_at(dot),
                _ => (clean.as_str(), ""),
            };
            let mut name = clean.clone();
            let mut count = 1;
            while !taken.insert(name.clone()) {
                count += 1;
                name = format!("{stem}-{count}{extension}");
            }
            name
        })
        .collect()
}
