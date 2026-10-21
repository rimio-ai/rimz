//! tmux control-mode presence watcher.

use std::io::{BufRead as _, Write as _};

// ── Control-mode presence stream ──────────────────────────────────────────────

const SUBSCRIPTION_COMMAND: &str = concat!(
    "refresh-client -B \"rimz-presence:%*:#{pane_id}",
    ",#{window_id}",
    ",#{s/,/_/g:pane_current_command}",
    ",#{pane_active}",
    ",#{s/,/_/g:pane_title}",
    ",#{pane_floating_flag}\"\n",
);
const SEED_COMMAND: &str = concat!(
    "list-panes -s -F \"rimz-presence-seed:#{pane_id}",
    ",#{window_id}",
    ",#{pane_left}",
    ",#{pane_width}",
    ",#{window_width}",
    ",#{session_id}",
    ",#{window_active}\"\n",
);
type LayoutGeometry = (u64, Vec<TmuxLayoutPane>);

/// A tmux control-mode line carrying pane-presence information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlLine {
    WindowAdded,
    Subscription {
        pane: String,
        window: String,
        command: Option<String>,
        active: bool,
        title: Option<String>,
        floating: bool,
    },
    SeedPane {
        pane: String,
        window: String,
        x: u64,
        width: u64,
        window_width: u64,
        session: String,
        window_active: bool,
    },
    WindowClosed {
        window: String,
    },
    LayoutChange {
        window: String,
        window_width: u64,
        /// Tiled panes, except in a zoomed window, where tmux writes no floating
        /// suffix and floating panes appear as leaves; floating panes arrive
        /// through the subscription.
        panes: Vec<TmuxLayoutPane>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxLayoutPane {
    pub id: String,
    pub x: u64,
    pub width: u64,
}

/// A live tmux control-mode presence stream — the tmux fast path for pane
/// topology and command/focus overlays (docs/internals/multiplexers.md).
/// Attaches a writable, size-excluded (`ignore-size`), output-suppressed
/// (`no-output`) control client to one session, registers one `refresh-client -B`
/// subscription, and surfaces typed presence changes. Writable keeps tmux
/// 3.7 `send-keys` usable when this watch is the sole attached client in a
/// headless session; safety lives in the stdin allowlist, which writes only the
/// subscription and pane-seed commands. Poll stays truth — a dropped stream
/// loses only latency, never correctness, and the consumer respawns it.
pub struct PresenceWatch {
    child: std::process::Child,
    lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    /// Held open for the stream's lifetime: a control client exits on stdin
    /// EOF, which doubles as the no-leak guarantee — if this process dies, the
    /// pipe closes and tmux reaps the client.
    _stdin: std::process::ChildStdin,
}

impl PresenceWatch {
    /// Attach a writable control client to `session` on `socket`, excluding it
    /// from size negotiation and pane output. `$TMUX` is dropped from the
    /// child's env so the nested attach is deliberate rather than refused.
    ///
    /// The socket is explicit rather than inherited: the watch always follows
    /// a named managed session, so resolving the endpoint from the ambient
    /// `$TMUX` would point it at the wrong server whenever the sidebar runs
    /// outside a managed pane.
    pub fn attach(socket: &std::path::Path, session: &str) -> std::io::Result<Self> {
        let mut cmd = std::process::Command::new("tmux");
        cmd.arg("-S").arg(socket).arg("-u");
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
        if let Err(err) = stdin.write_all(SEED_COMMAND.as_bytes()) {
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

/// Classify one tmux control-mode line. Reply blocks, pane output, and
/// identity-free focus noise are ignored; a window's active-pane notification
/// is forwarded for current-window focus projection, and topology notifications that
/// lack identity stay as a producer-verification nudge.
fn classify_control_line(line: &str) -> ControlLine {
    if line.starts_with("rimz-presence-seed:") {
        return parse_seed(line).unwrap_or(ControlLine::Ignore);
    }
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
        "%window-add" | "%unlinked-window-add" => ControlLine::WindowAdded,
        "%sessions-changed" => ControlLine::Nudge,
        _ => ControlLine::Ignore,
    }
}

fn parse_seed(line: &str) -> Option<ControlLine> {
    let value = line.strip_prefix("rimz-presence-seed:")?;
    let mut fields = value.splitn(7, ',');
    Some(ControlLine::SeedPane {
        pane: nonempty(fields.next()?)?,
        window: nonempty(fields.next()?)?,
        x: fields.next()?.trim().parse().ok()?,
        width: fields.next()?.trim().parse().ok()?,
        window_width: fields.next()?.trim().parse().ok()?,
        session: nonempty(fields.next()?)?,
        window_active: fields.next()?.trim() == "1",
    })
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
    let (window_width, panes) = layout_geometry(layout)?;
    Some(ControlLine::LayoutChange {
        window: window.to_owned(),
        window_width,
        panes,
    })
}

fn layout_geometry(layout: &str) -> Option<LayoutGeometry> {
    let layout = super::layout::Layout::parse(layout)?;
    let panes = layout
        .leaves()
        .into_iter()
        .map(|cell| {
            Some(TmuxLayoutPane {
                id: cell.pane_id()?.to_owned(),
                x: u64::try_from(cell.x).ok()?,
                width: cell.width,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((layout.root.width, panes))
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
                "rimz-presence-seed:%2,@1,55,79,213,$0,1",
                ControlLine::SeedPane {
                    pane: "%2".to_owned(),
                    window: "@1".to_owned(),
                    x: 55,
                    width: 79,
                    window_width: 213,
                    session: "$0".to_owned(),
                    window_active: true,
                },
            ),
            (
                "rimz-presence-seed:%3,@2,0,80,80,$0,0",
                ControlLine::SeedPane {
                    pane: "%3".to_owned(),
                    window: "@2".to_owned(),
                    x: 0,
                    width: 80,
                    window_width: 80,
                    session: "$0".to_owned(),
                    window_active: false,
                },
            ),
            ("rimz-presence-seed:%2,@1,55,79,213", ControlLine::Ignore),
            ("rimz-presence-seed:%2,@1,55,79,213,$0", ControlLine::Ignore),
            ("rimz-presence-seed:%2,@1,55,79,213,,1", ControlLine::Ignore),
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
            ("%window-add @5", ControlLine::WindowAdded),
            ("%unlinked-window-add @5", ControlLine::WindowAdded),
            ("%unlinked-window-add @6", ControlLine::WindowAdded),
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
    fn control_line_drops_floating_panes_from_layout_change() {
        let tiled = vec![
            TmuxLayoutPane {
                id: "%0".to_owned(),
                x: 0,
                width: 100,
            },
            TmuxLayoutPane {
                id: "%1".to_owned(),
                x: 101,
                width: 99,
            },
        ];
        // Captured from tmux 3.7c with two floating panes, `%2` and `%3`.
        assert_eq!(
            classify_control_line(
                "%layout-change @0 0bcf,200x50,0,0{100x50,0,0,0,99x50,101,0,1,40x10,4,2,2,30x8,8,4,3}<30x8,8,4,3,40x10,4,2,2> \
                 0bcf,200x50,0,0{100x50,0,0,0,99x50,101,0,1,40x10,4,2,2,30x8,8,4,3}<30x8,8,4,3,40x10,4,2,2> *"
            ),
            ControlLine::LayoutChange {
                window: "@0".to_owned(),
                window_width: 200,
                panes: tiled,
            }
        );
        for malformed in [
            "aabb,200x50,0,0,0<",
            "aabb,200x50,0,0,0<>",
            "aabb,200x50,0,0,0<30x8,8,4,3",
            "aabb,200x50,0,0,0<30x8,8,4,3>x",
        ] {
            assert_eq!(layout_geometry(malformed), None, "{malformed}");
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
                window_width: 208,
                panes: vec![
                    TmuxLayoutPane {
                        id: "%1".to_owned(),
                        x: 0,
                        width: 104,
                    },
                    TmuxLayoutPane {
                        id: "%2".to_owned(),
                        x: 105,
                        width: 103,
                    },
                ],
            }
        );
        assert_eq!(
            layout_geometry(
                "aabb,200x60,0,0{100x60,0,0[100x30,0,0,3,100x29,0,31,4],99x60,101,0,5}"
            ),
            Some((
                200,
                vec![
                    TmuxLayoutPane {
                        id: "%3".to_owned(),
                        x: 0,
                        width: 100,
                    },
                    TmuxLayoutPane {
                        id: "%4".to_owned(),
                        x: 0,
                        width: 100,
                    },
                    TmuxLayoutPane {
                        id: "%5".to_owned(),
                        x: 101,
                        width: 99,
                    },
                ],
            ))
        );
    }
}
