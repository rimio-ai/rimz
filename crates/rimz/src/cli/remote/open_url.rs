//! `rimz remote open-url`: the room's `BROWSER` helper.
//!
//! Tries, in order: the live laptop links of this room (most recently active
//! laptop first), then, where this machine can show a page, the user's own
//! `BROWSER` (the helper becomes it) and the platform opener, and last the
//! clipboard of the terminal the helper runs in. The relay and the platform
//! opener return as soon as they have the URL, because providers time out a slow
//! `BROWSER` program; the user's own browser takes as long as it would without
//! RimZ.

use std::io::{BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, bail};
use rimz::remote::browser::{self, OpenReply, OpenRequest, OpenStatus, WebUrl};

/// How long a link has to say it is there before the helper skips it.
const STATUS_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a link has to put the URL on its probe stream.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn run(url: &str) -> Result<()> {
    let web = browser::web_url(url);
    if let Ok(web) = &web
        && relay(web)
    {
        say(&format!("sent {url} to the attached laptop"));
        return Ok(());
    }
    let failed_exec = if crate::cli::ssh_without_display() {
        None
    } else {
        exec_user_browser(url)
    };
    let local = match crate::cli::launch_browser(url).opened() {
        Ok(opener) => {
            say(&format!("opened {url} with {opener}"));
            return Ok(());
        }
        Err(reason) => match failed_exec {
            Some(failed) => format!("{failed}, and {reason}"),
            None => reason,
        },
    };
    let web = match web {
        Ok(web) => web,
        Err(refusal) => bail!("no browser could open {url}: {local}; {refusal}"),
    };
    match copy_to_clipboard(&web) {
        Ok(()) => {
            say(&format!(
                "copied {url} to the clipboard: no attached laptop took it and {local}"
            ));
            Ok(())
        }
        Err(error) => bail!(
            "no browser could open {url}: no attached laptop took it, {local}, and the clipboard is out of reach ({error})"
        ),
    }
}

fn say(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "rimz: {message}");
}

/// Offer the URL to each live link of this room, most recently active first.
/// Each status is read on a connection of its own and each offer connects again,
/// so no link waits on the helper while it is busy with one ahead in the order.
fn relay(web: &WebUrl<'_>) -> bool {
    let Some(runtime) = room_runtime() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(&runtime.sock_dir) else {
        return false;
    };
    let links = entries
        .map_while(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(browser::socket_pid)
                .is_some()
        })
        .filter_map(|entry| {
            let path = entry.path();
            let (_, status) = connect(&path, STATUS_TIMEOUT)?;
            Some((path, status.active_at_ms))
        })
        .collect();
    browser::relay_order::<PathBuf>(links)
        .into_iter()
        .any(|path| offer(&path, web.url))
}

fn room_runtime() -> Option<rimz::RuntimePaths> {
    let cwd = std::env::current_dir().ok()?;
    let room = rimz::WorkspaceResolver::resolve_participant(&cwd, None).ok()?;
    let state = rimz::StatePaths::for_project_root(&room.project_root).ok()?;
    rimz::RuntimePaths::for_state(&state).ok()
}

/// A link that answers with its status within `timeout`. A socket nobody answers,
/// or an ingest whose laptop has gone quiet, which sends none, is skipped.
fn connect(path: &Path, timeout: Duration) -> Option<(BufReader<UnixStream>, OpenStatus)> {
    let stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
    let mut reader = BufReader::new(stream);
    let status: OpenStatus = read_json_line(&mut reader)?;
    status.version_ok().then_some((reader, status))
}

fn offer(path: &Path, url: &str) -> bool {
    let Some((mut reader, _)) = connect(path, REPLY_TIMEOUT) else {
        return false;
    };
    let Ok(mut request) = serde_json::to_vec(&OpenRequest::new(url)) else {
        return false;
    };
    request.push(b'\n');
    if reader.get_ref().write_all(&request).is_err() {
        return false;
    }
    read_json_line::<OpenReply>(&mut reader).is_some_and(|reply| reply.delivered())
}

fn read_json_line<T: serde::de::DeserializeOwned>(reader: &mut impl std::io::BufRead) -> Option<T> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Become the user's own `BROWSER` when the room saved one, trying each of its
/// entries in turn, so the caller sees that program's stdio and exit status as it
/// did before RimZ set `BROWSER`. Any scheme passes. Returns only when there is
/// none, or with the reason the last exec failed.
fn exec_user_browser(url: &str) -> Option<String> {
    use std::os::unix::process::CommandExt as _;

    let saved = std::env::var(browser::USER_BROWSER_ENV).unwrap_or_default();
    let mut failed = None;
    for argv in browser::user_browser_argvs(&saved, url) {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        rimz::child_process::restore_user_temp_env(&mut command);
        rimz::remote::browser::restore_user_browser_env(&mut command);
        let error = command.exec();
        failed = Some(format!("{} failed to start: {error}", argv[0]));
    }
    failed
}

/// Write OSC 52 to the helper's controlling terminal: providers capture its stdout.
fn copy_to_clipboard(web: &WebUrl<'_>) -> std::io::Result<()> {
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    tty.write_all(&browser::clipboard_sequence(web))?;
    tty.flush()
}
