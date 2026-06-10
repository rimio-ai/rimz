//! tmux control-mode presence watcher.

use std::path::PathBuf;

// ── Control-mode presence stream ──────────────────────────────────────────────

/// A live tmux control-mode presence stream — the tmux fast path for pane
/// topology (docs/internals/sidebar/multiplexers.md). Attaches a read-only (`-r`),
/// output-suppressed (`-f no-output`) control client to one session and
/// surfaces a nudge per presence-relevant notification: a window opened or
/// closed, a layout change (a split opened/closed inside a window). Poll stays
/// truth — a dropped stream loses only latency, never correctness, and the
/// consumer respawns it.
pub struct PresenceWatch {
    child: std::process::Child,
    lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    /// Held open for the stream's lifetime: a control client exits on stdin
    /// EOF, which doubles as the no-leak guarantee — if this process dies, the
    /// pipe closes and tmux reaps the client.
    _stdin: Option<std::process::ChildStdin>,
}

impl PresenceWatch {
    /// Attach a control client to `session` (on `socket` when given, else the
    /// default server). `$TMUX` is dropped from the child's env so the nested
    /// attach is deliberate rather than refused.
    pub fn attach(socket: Option<&std::path::Path>, session: &str) -> std::io::Result<Self> {
        use std::io::BufRead as _;
        let mut cmd = std::process::Command::new("tmux");
        if let Some(socket) = socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.args([
            "-C",
            "attach-session",
            "-r",
            "-f",
            "no-output",
            "-t",
            session,
        ])
        .env_remove("TMUX")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("tmux control client spawned without a stdout pipe")
        })?;
        Ok(Self {
            child,
            lines: std::io::BufReader::new(stdout).lines(),
            _stdin: stdin,
        })
    }

    /// Block until the next presence-relevant notification. `None` when the
    /// stream ends — the client was detached, the server exited, or the pipe
    /// broke — after which the watch is spent and the caller re-attaches.
    pub fn next_presence(&mut self) -> Option<()> {
        loop {
            let line = self.lines.next()?.ok()?;
            if is_presence_event(&line) {
                return Some(());
            }
        }
    }
}

impl Drop for PresenceWatch {
    fn drop(&mut self) {
        // Best-effort: the stdin pipe closing already detaches the client;
        // the kill only hurries a wedged one along.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The control-mode socket of the server this process is running inside, from
/// `$TMUX` (`<socket>,<pid>,<session-idx>`). `None` outside tmux.
pub fn control_socket_from_env() -> Option<PathBuf> {
    control_socket_from(&std::env::var("TMUX").ok()?)
}

pub(super) fn control_socket_from(raw: &str) -> Option<PathBuf> {
    let socket = raw.split(',').next()?.trim();
    (!socket.is_empty()).then(|| PathBuf::from(socket))
}

/// Whether a control-mode notification line reports a pane-topology change.
/// Only presence moves the sidebar: window add/close (linked or not) and
/// layout changes (a split opened/closed). Everything else — `%output`
/// (suppressed by `-f no-output` anyway), command replies (`%begin`/`%end`),
/// focus and mode changes — stays silent.
pub(super) fn is_presence_event(line: &str) -> bool {
    [
        "%window-add",
        "%unlinked-window-add",
        "%window-close",
        "%unlinked-window-close",
        "%layout-change",
        "%sessions-changed",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn presence_filter_accepts_topology_and_skips_noise() {
        for line in [
            "%window-add @5",
            "%unlinked-window-add @6",
            "%window-close @5",
            "%unlinked-window-close @6",
            "%layout-change @1 b25d,208x60,0,0{104x60,0,0,1,103x60,105,0,2}",
            "%sessions-changed",
        ] {
            assert!(is_presence_event(line), "{line}");
        }
        for line in [
            "%begin 1622 0 1",
            "%end 1622 0 1",
            "%output %1 aGVsbG8=",
            "%window-pane-changed @1 %2",
            "%client-session-changed /dev/pts/3 $1 main",
            "%pane-mode-changed %2",
            "%window-renamed @1 build",
            "",
        ] {
            assert!(!is_presence_event(line), "{line}");
        }
    }

    #[test]
    fn control_socket_parses_the_tmux_env_shape() {
        assert_eq!(
            control_socket_from("/tmp/tmux-1000/default,12345,0"),
            Some(PathBuf::from("/tmp/tmux-1000/default"))
        );
        assert_eq!(control_socket_from(""), None);
        assert_eq!(control_socket_from(",12345,0"), None);
    }
}
