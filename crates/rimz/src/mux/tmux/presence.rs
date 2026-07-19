//! tmux control-mode presence watcher.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;

// ── Control-mode presence stream ──────────────────────────────────────────────

const SUBSCRIPTION_COMMAND: &str = concat!(
    "refresh-client -B \"rimz-presence:%*:#{pane_id}",
    ",#{window_id}",
    ",#{s/,/_/g:pane_current_command}",
    ",#{pane_active}",
    ",#{s/,/_/g:pane_title}",
    ",#{pane_floating_flag}\"\n",
);

/// A tmux control-mode line carrying pane-presence information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlLine {
    Subscription {
        pane: String,
        window: String,
        command: Option<String>,
        active: bool,
        title: Option<String>,
        floating: bool,
    },
    WindowClosed {
        window: String,
    },
    LayoutChange {
        window: String,
        panes: Vec<String>,
    },
    WindowPaneChanged {
        window: String,
        pane: String,
    },
    SessionWindowChanged {
        session: String,
        window: String,
    },
    Nudge,
    Ignore,
}

/// A live tmux control-mode presence stream — the tmux fast path for pane
/// topology and command/focus overlays (docs/internals/multiplexers.md).
/// Attaches a writable, size-excluded (`ignore-size`), output-suppressed
/// (`no-output`) control client to one session, registers one `refresh-client -B`
/// subscription, and surfaces typed presence changes. Writable keeps tmux
/// 3.7 `send-keys` usable when this watch is the sole attached client in a
/// headless session; safety lives in the stdin allowlist, which writes only the
/// subscription command. Poll stays truth — a dropped stream loses only latency,
/// never correctness, and the consumer respawns it.
pub struct PresenceWatch {
    child: std::process::Child,
    lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    /// Held open for the stream's lifetime: a control client exits on stdin
    /// EOF, which doubles as the no-leak guarantee — if this process dies, the
    /// pipe closes and tmux reaps the client.
    _stdin: std::process::ChildStdin,
}

impl PresenceWatch {
    /// Attach a writable control client to `session` (on `socket` when given,
    /// else the default server), excluding it from size negotiation and pane
    /// output. `$TMUX` is dropped from the child's env so the nested attach is
    /// deliberate rather than refused.
    pub fn attach(socket: Option<&std::path::Path>, session: &str) -> std::io::Result<Self> {
        let mut cmd = std::process::Command::new("tmux");
        if let Some(socket) = socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.args([
            "-C",
            "attach-session",
            "-f",
            "ignore-size,no-output",
            "-t",
            session,
        ])
        .env_remove("TMUX")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn()?;
        let mut stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(
                    "tmux control client spawned without a stdin pipe",
                ));
            }
        };
        if let Err(err) = stdin.write_all(SUBSCRIPTION_COMMAND.as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(
                    "tmux control client spawned without a stdout pipe",
                ));
            }
        };
        Ok(Self {
            child,
            lines: std::io::BufReader::new(stdout).lines(),
            _stdin: stdin,
        })
    }

    /// Block until the next presence-relevant control line. `None` when the
    /// stream ends — the client was detached, the server exited, or the pipe
    /// broke — after which the watch is spent and the caller re-attaches.
    pub fn next_line(&mut self) -> Option<ControlLine> {
        loop {
            let line = self.lines.next()?.ok()?;
            let classified = classify_control_line(&line);
            if classified != ControlLine::Ignore {
                return Some(classified);
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
    super::socket_path_from_tmux_var(&std::env::var("TMUX").ok()?)
}

/// Classify one tmux control-mode line. Reply blocks, pane output, and
/// identity-free focus noise are ignored; a window's active-pane notification
/// is forwarded as a realtime focus overlay, and topology notifications that
/// lack identity stay as a producer-verification nudge.
pub(super) fn classify_control_line(line: &str) -> ControlLine {
    let verb = line.split_whitespace().next().unwrap_or_default();
    match verb {
        "%subscription-changed" => parse_subscription(line).unwrap_or(ControlLine::Ignore),
        "%window-close" | "%unlinked-window-close" => line
            .split_whitespace()
            .nth(1)
            .filter(|window| window.starts_with('@'))
            .map(|window| ControlLine::WindowClosed {
                window: window.to_owned(),
            })
            .unwrap_or(ControlLine::Nudge),
        "%layout-change" => parse_layout_change(line).unwrap_or(ControlLine::Nudge),
        "%window-pane-changed" => parse_window_pane_changed(line).unwrap_or(ControlLine::Ignore),
        "%session-window-changed" => {
            parse_session_window_changed(line).unwrap_or(ControlLine::Ignore)
        }
        "%window-add" | "%unlinked-window-add" | "%sessions-changed" => ControlLine::Nudge,
        _ => ControlLine::Ignore,
    }
}

fn parse_subscription(line: &str) -> Option<ControlLine> {
    let (_, value) = line.split_once(" : ")?;
    let mut fields = value.splitn(6, ',');
    let pane = nonempty(fields.next()?)?;
    let window = nonempty(fields.next()?)?;
    let command = nonempty(fields.next()?);
    let active = fields.next()?.trim() == "1";
    let title = nonempty(fields.next().unwrap_or_default());
    let floating = fields.next().is_some_and(|value| value.trim() == "1");
    Some(ControlLine::Subscription {
        pane,
        window,
        command,
        active,
        title,
        floating,
    })
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_session_window_changed(line: &str) -> Option<ControlLine> {
    let mut fields = line.split_whitespace();
    (fields.next()? == "%session-window-changed").then_some(())?;
    let session = fields.next().filter(|value| value.starts_with('$'))?;
    let window = fields.next().filter(|value| value.starts_with('@'))?;
    Some(ControlLine::SessionWindowChanged {
        session: session.to_owned(),
        window: window.to_owned(),
    })
}

fn parse_window_pane_changed(line: &str) -> Option<ControlLine> {
    let mut fields = line.split_whitespace();
    (fields.next()? == "%window-pane-changed").then_some(())?;
    let window = fields.next().filter(|value| value.starts_with('@'))?;
    let pane = fields.next().filter(|value| value.starts_with('%'))?;
    Some(ControlLine::WindowPaneChanged {
        window: window.to_owned(),
        pane: pane.to_owned(),
    })
}

fn parse_layout_change(line: &str) -> Option<ControlLine> {
    let mut fields = line.split_whitespace();
    (fields.next()? == "%layout-change").then_some(())?;
    let window = fields.next()?;
    let layout = fields.next()?;
    let panes = layout_pane_ids(layout)?;
    Some(ControlLine::LayoutChange {
        window: window.to_owned(),
        panes,
    })
}

fn layout_pane_ids(layout: &str) -> Option<Vec<String>> {
    let (_, tree) = layout.split_once(',')?;
    let mut parser = LayoutParser {
        input: tree.as_bytes(),
        pos: 0,
        panes: Vec::new(),
    };
    parser.parse_cell()?;
    (parser.pos == parser.input.len()).then_some(parser.panes)
}

struct LayoutParser<'a> {
    input: &'a [u8],
    pos: usize,
    panes: Vec<String>,
}

impl LayoutParser<'_> {
    fn parse_cell(&mut self) -> Option<()> {
        self.parse_number()?;
        self.consume(b'x')?;
        self.parse_number()?;
        self.consume(b',')?;
        self.parse_number()?;
        self.consume(b',')?;
        self.parse_number()?;
        match self.peek()? {
            b'{' | b'[' => self.parse_group(),
            b',' => {
                self.pos += 1;
                let pane = self.parse_number()?;
                self.panes.push(format!("%{pane}"));
                Some(())
            }
            _ => None,
        }
    }

    fn parse_group(&mut self) -> Option<()> {
        let opener = self.next()?;
        let closer = match opener {
            b'{' => b'}',
            b'[' => b']',
            _ => return None,
        };
        loop {
            self.parse_cell()?;
            match self.peek()? {
                b',' => {
                    self.pos += 1;
                }
                byte if byte == closer => {
                    self.pos += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    fn parse_number(&mut self) -> Option<u64> {
        let start = self.pos;
        while self
            .input
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    fn consume(&mut self, expected: u8) -> Option<()> {
        (self.next()? == expected).then_some(())
    }

    fn next(&mut self) -> Option<u8> {
        let byte = *self.input.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn control_line_classifies_tmux_events_and_skips_noise() {
        let cases = vec![
            (
                "%subscription-changed rimz-presence $0 @1 1 %2 : %2,@1,claude,1,rimz,1",
                ControlLine::Subscription {
                    pane: "%2".to_owned(),
                    window: "@1".to_owned(),
                    command: Some("claude".to_owned()),
                    active: true,
                    title: Some("rimz".to_owned()),
                    floating: true,
                },
            ),
            (
                "%subscription-changed rimz-presence $0 @1 1 %2 : %2,@1,,0,",
                ControlLine::Subscription {
                    pane: "%2".to_owned(),
                    window: "@1".to_owned(),
                    command: None,
                    active: false,
                    title: None,
                    floating: false,
                },
            ),
            ("%window-add @5", ControlLine::Nudge),
            ("%unlinked-window-add @6", ControlLine::Nudge),
            (
                "%window-close @5",
                ControlLine::WindowClosed {
                    window: "@5".to_owned(),
                },
            ),
            (
                "%unlinked-window-close @6",
                ControlLine::WindowClosed {
                    window: "@6".to_owned(),
                },
            ),
            ("%sessions-changed", ControlLine::Nudge),
            (
                "%session-window-changed $1 @2",
                ControlLine::SessionWindowChanged {
                    session: "$1".to_owned(),
                    window: "@2".to_owned(),
                },
            ),
            ("%session-window-changed $1", ControlLine::Ignore),
            (
                "%window-pane-changed @1 %2",
                ControlLine::WindowPaneChanged {
                    window: "@1".to_owned(),
                    pane: "%2".to_owned(),
                },
            ),
            ("%window-pane-changed @1", ControlLine::Ignore),
            (
                "%layout-change @1 b25d,208x60,0,0{104x60,0,0",
                ControlLine::Nudge,
            ),
            ("%begin 1622 0 1", ControlLine::Ignore),
            ("%end 1622 0 1", ControlLine::Ignore),
            ("%error 1622 0 1", ControlLine::Ignore),
            ("%output %1 aGVsbG8=", ControlLine::Ignore),
            (
                "%client-session-changed /dev/pts/3 $1 main",
                ControlLine::Ignore,
            ),
            ("%pane-mode-changed %2", ControlLine::Ignore),
            ("%window-renamed @1 build", ControlLine::Ignore),
            ("", ControlLine::Ignore),
        ];

        for (line, expected) in cases {
            assert_eq!(classify_control_line(line), expected, "{line}");
        }
    }

    #[test]
    fn control_line_extracts_leaf_panes_from_layout_change() {
        assert_eq!(
            classify_control_line(
                "%layout-change @1 b25d,208x60,0,0{104x60,0,0,1,103x60,105,0,2} 208x60,0,0 0"
            ),
            ControlLine::LayoutChange {
                window: "@1".to_owned(),
                panes: vec!["%1".to_owned(), "%2".to_owned()],
            }
        );
        assert_eq!(
            layout_pane_ids(
                "aabb,200x60,0,0{100x60,0,0[100x30,0,0,3,100x29,0,31,4],99x60,101,0,5}"
            ),
            Some(vec!["%3".to_owned(), "%4".to_owned(), "%5".to_owned()])
        );
    }
}
