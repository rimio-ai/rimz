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

/// Run `rimz` with `argv` on a worker. The child inherits the host's
/// environment, which already carries no pane identity.
pub(super) fn spawn_rimz(
    runtime: &RuntimePaths,
    argv: Vec<String>,
    wake: Option<SyncSender<Wakeup>>,
) {
    #[cfg(test)]
    {
        let _ = (runtime, wake);
        SPAWNED.with(|spawned| spawned.borrow_mut().push(argv));
    }
    #[cfg(not(test))]
    {
        let mut command = Command::new(crate::proc::rimz_exe());
        crate::child_process::anchor_detached_command(&mut command, runtime).args(argv);
        run_reporting(command, wake);
    }
}

/// Run `command` on a worker and report its first stderr line, or a non-zero
/// exit with none, through `wake`. Stderr counts whatever the exit: `rimz
/// sidebar close` prints a host refusal there and still exits 0.
fn run_reporting(mut command: Command, wake: Option<SyncSender<Wakeup>>) {
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
                    Some(line) => line,
                    None if output.status.success() => return,
                    None => format!("rimz exited with {}", output.status),
                }
            }
            Err(err) => format!("rimz did not start: {err}"),
        };
        if let Some(wake) = wake {
            let _ = wake.try_send(Wakeup::Notice(reason));
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
        run_reporting(failing, Some(wake.clone()));
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(10)),
            Ok(Wakeup::Notice("no client attached".into()))
        );
        let mut refused = Command::new("sh");
        refused.args(["-c", "echo 'sidebar close: no card owns pane tmux:%9' >&2"]);
        run_reporting(refused, Some(wake.clone()));
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(10)),
            Ok(Wakeup::Notice(
                "sidebar close: no card owns pane tmux:%9".into()
            )),
            "a refusal printed before a clean exit still reaches the pane"
        );
        let mut clean = Command::new("sh");
        clean.args(["-c", "true"]);
        run_reporting(clean, Some(wake));
        assert!(
            notices.recv_timeout(Duration::from_secs(10)).is_err(),
            "a clean, quiet exit sends nothing and the worker drops its sender"
        );
    }
}
