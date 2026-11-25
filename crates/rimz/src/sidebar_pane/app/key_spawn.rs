//! Key effects that run a `rimz` command (`dd`'s close, the palette popup of
//! `:`, `o`, `O` and `.`) on a worker thread, off the views lock and the frame
//! loop. A run that writes to stderr or exits non-zero sends its first stderr
//! line back to the pane as a [`Wakeup::Notice`].

use std::process::{Command, Stdio};
use std::sync::mpsc::SyncSender;

use crate::disk::paths::RuntimePaths;
use crate::ids::{MuxName, PaneId};

use super::input::Wakeup;

/// The palette popup's argv: the mode `args`, then the room, the card it
/// starts on and, on tmux, the client to open on.
pub(super) fn palette_argv(
    args: &[String],
    session: &str,
    mux: MuxName,
    from_pane: Option<&PaneId>,
    client: Option<&str>,
) -> Vec<String> {
    let mut argv = vec!["palette".to_owned()];
    argv.extend_from_slice(args);
    argv.extend(["--popup", "--session-name", session, "--mux", mux.as_str()].map(str::to_owned));
    if let Some(pane) = from_pane {
        argv.extend(["--from-pane".to_owned(), pane.to_string()]);
    }
    if let Some(client) = client {
        argv.extend(["--client".to_owned(), client.to_owned()]);
    }
    argv
}

/// `rimz sidebar close` for the card that owns `pane`.
pub(super) fn close_argv(pane: &PaneId, session: &str, mux: MuxName) -> Vec<String> {
    let pane = pane.to_string();
    [
        "sidebar",
        "close",
        "--pane",
        &pane,
        "--session-name",
        session,
        "--mux",
        mux.as_str(),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `rimz agents resume <session_id>` for a paneless reopenable child.
pub(super) fn reopen_argv(session_id: &str) -> Vec<String> {
    ["agents", "resume", session_id].map(str::to_owned).to_vec()
}

/// Run `rimz` with `argv` on a worker. The child inherits the host's
/// environment, which already carries no pane identity. When `reopen_id` is
/// `Some`, completion always reports [`Wakeup::ReopenFinished`] so the view
/// clears its in-flight mark and surfaces any first stderr line as the notice.
pub(super) fn spawn_rimz(
    runtime: &RuntimePaths,
    argv: Vec<String>,
    wake: Option<SyncSender<Wakeup>>,
    reopen_id: Option<String>,
) {
    #[cfg(test)]
    {
        let _ = (runtime, wake, reopen_id);
        SPAWNED.with(|spawned| spawned.borrow_mut().push(argv));
    }
    #[cfg(not(test))]
    {
        let mut command = Command::new(crate::proc::rimz_exe());
        crate::child_process::anchor_detached_command(&mut command, runtime).args(argv);
        run_reporting(command, wake, reopen_id);
    }
}

/// Run `command` on a worker and report its first stderr line, or a non-zero
/// exit with none, through `wake`. Stderr counts whatever the exit: `rimz
/// sidebar close` prints a refusal there, from the host or from its own read
/// of the room, and still exits 0. When `reopen_id` is `Some`, every exit
/// sends [`Wakeup::ReopenFinished`] with any refusal line attached.
fn run_reporting(
    mut command: Command,
    wake: Option<SyncSender<Wakeup>>,
    reopen_id: Option<String>,
) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    std::thread::spawn(move || {
        let reason = match command.output() {
            Ok(output) => {
                let first = String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned);
                match first {
                    Some(line) => Some(line),
                    None if output.status.success() => None,
                    None => Some(format!("rimz exited with {}", output.status)),
                }
            }
            Err(err) => Some(format!("rimz did not start: {err}")),
        };
        if let Some(wake) = wake {
            if let Some(id) = reopen_id {
                let _ = wake.try_send(Wakeup::ReopenFinished { id, notice: reason });
            } else if let Some(reason) = reason {
                let _ = wake.try_send(Wakeup::Notice(reason));
            }
        }
    });
}

#[cfg(test)]
thread_local! {
    static SPAWNED: std::cell::RefCell<Vec<Vec<String>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The argv each `spawn_rimz` on this thread ran, oldest first.
#[cfg(test)]
pub(super) fn take_spawned() -> Vec<Vec<String>> {
    SPAWNED.with(|spawned| std::mem::take(&mut *spawned.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn palette_argv_adds_the_room_the_card_and_only_a_named_client() {
        let mode = strings(&["--mode", "commands"]);
        let card = PaneId::from_parts(MuxName::Tmux, "%4");
        assert_eq!(
            palette_argv(
                &mode,
                "rimz-demo",
                MuxName::Tmux,
                Some(&card),
                Some("/dev/pts/3")
            ),
            strings(&[
                "palette",
                "--mode",
                "commands",
                "--popup",
                "--session-name",
                "rimz-demo",
                "--mux",
                "tmux",
                "--from-pane",
                "tmux:%4",
                "--client",
                "/dev/pts/3",
            ])
        );
        assert_eq!(
            palette_argv(&mode, "rimz-demo", MuxName::Tmux, None, None),
            strings(&[
                "palette",
                "--mode",
                "commands",
                "--popup",
                "--session-name",
                "rimz-demo",
                "--mux",
                "tmux",
            ])
        );
    }

    #[test]
    fn close_argv_names_the_pane_and_the_room() {
        let pane = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        assert_eq!(
            close_argv(&pane, "rimz-demo", MuxName::Zellij),
            strings(&[
                "sidebar",
                "close",
                "--pane",
                "zellij:terminal_3",
                "--session-name",
                "rimz-demo",
                "--mux",
                "zellij",
            ])
        );
    }

    #[test]
    fn a_run_reports_its_first_stderr_line_and_a_quiet_clean_one_nothing() {
        let (wake, notices) = std::sync::mpsc::sync_channel(4);
        let mut failing = Command::new("sh");
        failing.args([
            "-c",
            "printf '\\nno client attached\\nsecond\\n' >&2; exit 2",
        ]);
        run_reporting(failing, Some(wake.clone()), None);
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(10)),
            Ok(Wakeup::Notice("no client attached".into()))
        );
        let mut refused = Command::new("sh");
        refused.args([
            "-c",
            "echo 'sidebar close: pane tmux:%9 is not in session room; nothing was closed' >&2",
        ]);
        run_reporting(refused, Some(wake.clone()), None);
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(10)),
            Ok(Wakeup::Notice(
                "sidebar close: pane tmux:%9 is not in session room; nothing was closed".into()
            )),
            "a refusal printed before a clean exit still reaches the pane"
        );
        let mut reopen_err = Command::new("sh");
        reopen_err.args([
            "-c",
            "echo 'cannot reopen @coder: parent ended' >&2; exit 1",
        ]);
        run_reporting(reopen_err, Some(wake.clone()), Some("child-1".into()));
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(10)),
            Ok(Wakeup::ReopenFinished {
                id: "child-1".into(),
                notice: Some("cannot reopen @coder: parent ended".into()),
            })
        );
        let mut reopen_ok = Command::new("sh");
        reopen_ok.args(["-c", "true"]);
        run_reporting(reopen_ok, Some(wake.clone()), Some("child-2".into()));
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(10)),
            Ok(Wakeup::ReopenFinished {
                id: "child-2".into(),
                notice: None,
            })
        );
        let mut clean = Command::new("sh");
        clean.args(["-c", "true"]);
        run_reporting(clean, Some(wake), None);
        assert!(
            notices.recv_timeout(Duration::from_secs(10)).is_err(),
            "a clean, quiet exit sends nothing and the worker drops its sender"
        );
    }
}
